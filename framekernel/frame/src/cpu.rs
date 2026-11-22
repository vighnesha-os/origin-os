//! The kernel's own GDT, TSS and IDT, and CPU exception dispatch: the first M2 step
//! (CLAUDE.md §24; Intel SDM Vol. 3A §3.4, §6.10–§6.14, §7.2, §7.7).
//!
//! The descriptor encodings live in the library (`crate::descriptors`) and are
//! unit-tested there; this module only places them in memory and loads them. Exception
//! entry uses one naked stub per vector, which normalises the frame (pushing a dummy error
//! code where the processor does not push one, then the vector number), and a common stub
//! that saves the general-purpose registers and calls [`dispatch`] with System V alignment.
//!
//! It also switches to the kernel's own page tables ([`load_page_tables`]) and provides
//! *probes*: single deliberate accesses that report a page fault instead of taking it, which
//! prove on the real processor that the page protections hold.
//!
//! # The flags, and `preserves_flags`
//!
//! An `asm!` block declares `preserves_flags` only if every instruction in it is on the list
//! that `cargo xtask check` keeps (`developer-tools/xtask/src/asmflags.rs`): the instructions
//! that both Intel's and AMD's manuals say change no status flag (CLAUDE.md §13). A move to or
//! from a control or debug register is not on it: AMD says it affects no flag, and Intel says it
//! leaves all six status flags undefined (`docs/research/2026-11-10-asm-flags.md`). Without the
//! option the compiler assumes the block may change the flags, which is always true.

use core::arch::{asm, naked_asm};
use core::ptr;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::apic::{SPURIOUS_VECTOR, TIMER_VECTOR, X2APIC_EOI};
use crate::descriptors::{
    self as d, DOUBLE_FAULT_IST, GDT_ENTRIES, KERNEL_CODE, KERNEL_DATA, MACHINE_CHECK_IST, NMI_IST,
    TSS, TSS_SIZE,
};
use origin_sync::contexts::{BOOT_FLOW, Contexts, Moves, Quiet, Refused, Switched};

use crate::percpu::PerCpu;
use crate::trap::Switch;

/// Size of each interrupt stack (#DF, NMI, #MC, #DB).
pub const IST_STACK_SIZE: usize = 16 * 1024;
/// IST slot used for #DB: a debug exception can be taken with no usable kernel stack (a pending
/// one is delivered on the first instruction of `syscall_entry`, still on the user's).
const DEBUG_IST: u8 = 4;
/// The word of the TSS image where IST4 begins: byte 60 (`descriptors::tss_image` fills
/// IST1-IST3 from byte 36).
const TSS_IST4_WORD: usize = 15;

#[repr(C, align(16))]
struct Gdt([u64; GDT_ENTRIES]);

#[repr(C, align(16))]
struct TssImage([u32; 26]);

#[repr(C, align(4096))]
struct Idt([[u64; 2]; 256]);

/// One of the boot processor's interrupt stacks and the page below it. That page is unmapped
/// once the kernel's own page tables are live (`smp::start`), so running off the end of the
/// stack faults instead of overwriting what lies below: another stack, or the kernel's data.
#[repr(C, align(4096))]
struct IstStack {
    guard: [u8; 4096],
    stack: [u8; IST_STACK_SIZE],
}

const ZERO_IST_STACK: IstStack = IstStack {
    guard: [0; 4096],
    stack: [0; IST_STACK_SIZE],
};

/// The boot processor's interrupt stacks, in the kernel's image: they must exist from the first
/// boot step, before the kernel's tables or the stack region do. An application processor's are
/// in the stack region (`smp`, ADR-0004 Amendment 1, A2).
#[repr(C, align(4096))]
struct BspStacks {
    double_fault: IstStack,
    nmi: IstStack,
    machine_check: IstStack,
    debug: IstStack,
}

/// The operand of `lgdt` and `lidt`: a 16-bit limit followed by a 64-bit base.
#[repr(C, packed)]
struct TablePointer {
    limit: u16,
    base: u64,
}

/// One processor's GDT and TSS, on a page of their own. The IDT is shared, and the interrupt
/// stacks are elsewhere: the boot processor's in [`BspStacks`], an application processor's in
/// the stack region.
#[repr(C, align(4096))]
pub struct CpuTables {
    gdt: Gdt,
    tss: TssImage,
}

impl CpuTables {
    /// All zero, as an AP's tables are before `init_ap` writes them: integers only, so valid.
    pub const ZERO: Self = Self {
        gdt: Gdt([0; GDT_ENTRIES]),
        tss: TssImage([0; 26]),
    };
}

impl core::fmt::Debug for CpuTables {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CpuTables").finish_non_exhaustive()
    }
}

/// The boot processor's tables. Application processors get theirs from the heap.
static mut BSP_TABLES: CpuTables = CpuTables {
    gdt: Gdt([0; GDT_ENTRIES]),
    tss: TssImage([0; 26]),
};
/// The boot processor's interrupt stacks.
static mut BSP_STACKS: BspStacks = BspStacks {
    double_fault: ZERO_IST_STACK,
    nmi: ZERO_IST_STACK,
    machine_check: ZERO_IST_STACK,
    debug: ZERO_IST_STACK,
};
static mut IDT: Idt = Idt([[0; 2]; 256]);

/// The address of the field at `offset` in [`BSP_STACKS`]: no reference is made.
fn bsp_stacks_at(offset: usize) -> u64 {
    (&raw const BSP_STACKS).addr() as u64 + offset as u64
}

/// The tops of the boot processor's interrupt stacks: #DF, NMI, #MC, #DB.
fn bsp_interrupt_stack_tops() -> [u64; 4] {
    use core::mem::offset_of;
    let top = |offset: usize| bsp_stacks_at(offset) + IST_STACK_SIZE as u64;
    [
        top(offset_of!(BspStacks, double_fault.stack)),
        top(offset_of!(BspStacks, nmi.stack)),
        top(offset_of!(BspStacks, machine_check.stack)),
        top(offset_of!(BspStacks, debug.stack)),
    ]
}

/// Unmaps the guard page below each of the boot processor's four interrupt stacks and returns
/// their addresses (`smp::guard_interrupt_stacks` probes each one).
///
/// # Safety
///
/// Nothing may use the guard pages: the boot processor's interrupt stacks lie above them and
/// have not been used down that far, and nothing else ever touches a guard page.
pub(crate) unsafe fn protect_interrupt_stacks() -> Result<[u64; 4], &'static str> {
    let guards = interrupt_stack_guards();
    crate::vm::make_guards(&guards).map_err(|_| "cannot unmap the guard pages")?;
    Ok(guards)
}

/// The guard pages of the boot processor's four interrupt stacks.
pub(crate) fn interrupt_stack_guards() -> [u64; 4] {
    use core::mem::offset_of;
    [
        bsp_stacks_at(offset_of!(BspStacks, double_fault.guard)),
        bsp_stacks_at(offset_of!(BspStacks, nmi.guard)),
        bsp_stacks_at(offset_of!(BspStacks, machine_check.guard)),
        bsp_stacks_at(offset_of!(BspStacks, debug.guard)),
    ]
}

/// Registers saved by [`exception_common`], then the vector and error code the stubs push,
/// then the frame the processor pushed (SDM Vol. 3A Figure 6-9).
#[repr(C)]
struct ExceptionFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    vector: u64,
    error_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

/// Loads the GDT (null, kernel code, kernel data, TSS), reloads every segment register,
/// loads the TSS with a dedicated double-fault stack in IST1, and loads an IDT whose 32
/// exception vectors go to [`dispatch`].
///
/// # Safety
///
/// Call once, at CPL 0, on the boot processor, with interrupts disabled, before anything
/// else depends on the firmware's GDT or IDT.
pub unsafe fn init() {
    let mut idt = Idt([[0; 2]; 256]);
    for (vector, (gate, stub)) in idt.0.iter_mut().zip(STUBS.iter()).enumerate() {
        *gate = d::interrupt_gate(*stub as usize as u64, KERNEL_CODE, ist_of(vector as u64));
    }
    let interrupts: [(u8, extern "sysv64" fn()); 12] = [
        (TIMER_VECTOR, timer_stub),
        (SPURIOUS_VECTOR, spurious_stub),
        (YIELD_VECTOR, yield_stub),
        (SLEEP_VECTOR, sleep_stub),
        (EXIT_VECTOR, exit_stub),
        (BLOCK_VECTOR, block_stub),
        (crate::tlb::SHOOTDOWN_VECTOR, shootdown_stub),
        (RESCHEDULE_VECTOR, reschedule_stub),
        (DEVICE_VECTOR, device_stub),
        (NET_VECTOR, net_stub),
        (NET_TX_VECTOR, net_tx_stub),
        (IOAPIC_VECTOR, ioapic_stub),
    ];
    for (vector, stub) in interrupts {
        if let Some(gate) = idt.0.get_mut(usize::from(vector)) {
            *gate = d::interrupt_gate(stub as usize as u64, KERNEL_CODE, 0);
        }
    }
    // SAFETY: the IDT is written once, here, before any `lidt` points a processor at it
    // (the caller's contract: single-threaded, first).
    unsafe { (&raw mut IDT).write(idt) };
    // The boot processor is CPU index 0, whatever its APIC ID.
    assign(CpuIndex::BOOT, cpuid_apic_id());
    // SAFETY: `BSP_TABLES` is used only by the boot processor, and only from here on; so are
    // `BSP_STACKS`, whose tops these are.
    unsafe {
        load_tables(
            &raw mut BSP_TABLES,
            CpuIndex::BOOT,
            bsp_interrupt_stack_tops(),
        )
    };
    TABLES_READY.store(true, Ordering::Release);
}

/// Set when [`init`] has loaded the kernel's IDT: until then every vector would go to the
/// firmware's, so nothing that can let an interrupt in runs.
static TABLES_READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Whether the kernel's own IDT is loaded.
pub fn tables_ready() -> bool {
    TABLES_READY.load(Ordering::Acquire)
}

/// Loads an application processor's own GDT and TSS from `tables`, and the shared IDT, and
/// makes it CPU `index` (if its APIC ID is the one [`assign`]ed to `index`). `interrupt_stacks`
/// are the tops of its four interrupt stacks: #DF, NMI, #MC and #DB.
///
/// # Safety
///
/// Call once per application processor, at CPL 0 with interrupts disabled, after [`init`]
/// ran on the boot processor. `tables` must be writable, live for ever, and used by no
/// other processor; no other processor may be CPU `index`. Each of `interrupt_stacks` must be
/// the top of a stack of [`IST_STACK_SIZE`] bytes mapped read/write for ever, used by no other
/// processor and by nothing else on this one.
pub unsafe fn init_ap(tables: *mut CpuTables, index: CpuIndex, interrupt_stacks: [u64; 4]) {
    // SAFETY: the caller's contract is `load_tables`'s.
    unsafe { load_tables(tables, index, interrupt_stacks) };
}

/// Writes the GDT and TSS in `tables`, with `interrupt_stacks` in the TSS (#DF, NMI and #MC in
/// IST1–3, #DB in IST4), and loads them, the segment registers and the IDT.
///
/// # Safety
///
/// As for [`init_ap`]; for the boot processor, `tables` is `BSP_TABLES`, `index` is
/// [`CpuIndex::BOOT`] and `interrupt_stacks` are the tops of `BSP_STACKS`'s.
unsafe fn load_tables(tables: *mut CpuTables, index: CpuIndex, interrupt_stacks: [u64; 4]) {
    // SAFETY: the caller's contract: `tables` is valid, writable and this processor's own.
    // Projecting to a field creates no reference.
    let tss_field = unsafe { &raw mut (*tables).tss };
    // SAFETY: as above.
    let gdt_ptr_raw = unsafe { &raw mut (*tables).gdt };
    let [double_fault, nmi, machine_check, debug_top] = interrupt_stacks;
    let mut tss_image = TssImage(d::tss_image([double_fault, nmi, machine_check]));
    if let Some([low, high]) = tss_image
        .0
        .get_mut(TSS_IST4_WORD..TSS_IST4_WORD + 2)
        .and_then(|w| w.first_chunk_mut::<2>())
    {
        *low = debug_top as u32;
        *high = (debug_top >> 32) as u32;
    }
    // SAFETY: as above; the TSS is written before `ltr` loads it.
    unsafe { tss_field.write(tss_image) };
    let tss = d::tss_descriptor(tss_field.addr() as u64);
    let gdt = Gdt([
        0,
        d::kernel_code_segment(),
        d::kernel_data_segment(),
        tss[0],
        tss[1],
        d::user_data_segment(),
        d::user_code_segment(),
    ]);
    // SAFETY: as above; the GDT is written before `lgdt` points the processor at it.
    unsafe { gdt_ptr_raw.write(gdt) };

    let gdt_ptr = TablePointer {
        limit: (GDT_ENTRIES * 8 - 1) as u16,
        base: gdt_ptr_raw.addr() as u64,
    };
    let idt_ptr = TablePointer {
        limit: (256 * 16 - 1) as u16,
        base: (&raw const IDT).addr() as u64,
    };
    // SAFETY: `gdt_ptr` describes the fully written GDT above. After `lgdt`, CS is reloaded
    // with a far return to the next instruction and the data segments with the kernel data
    // selector, so no register keeps a selector into an older GDT. `ltr` loads the TSS
    // descriptor written above (type "available"). Interrupts stay disabled throughout.
    unsafe {
        asm!(
            "lgdt [{gdt}]",
            "push {code}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            // Never reached: the far return goes to `2:` (ADR-0017 Z10).
            "int3",
            "2:",
            "mov ds, {data:x}",
            "mov es, {data:x}",
            "mov fs, {data:x}",
            "mov gs, {data:x}",
            "mov ss, {data:x}",
            "ltr {tss:x}",
            gdt = in(reg) &raw const gdt_ptr,
            code = in(reg) u64::from(KERNEL_CODE),
            data = in(reg) u64::from(KERNEL_DATA),
            tss = in(reg) u64::from(TSS),
            // Written before the last input is read: it must not share a register with one.
            tmp = out(reg) _,
        );
    }
    // SAFETY: `idt_ptr` describes the IDT `init` wrote; every present gate points at a stub
    // in the kernel code segment just loaded.
    unsafe { asm!("lidt [{idt}]", idt = in(reg) &raw const idt_ptr, options(nostack)) };
    let _ = TSS_SIZE; // the limit is encoded by `tss_descriptor`
    // GS was just reloaded (base 0): point it at this processor's CPU-local area.
    init_cpu_local(tss_field.addr() as u64, index);
    init_cpu_state();
    // The index just set, read in a section (ADR-0020 S3; TASKS item 93): interrupts are already
    // off here, as the caller's contract says, so the section only gives the witness.
    without_interrupts_here(|at| init_speculation(this_cpu(at)));
}

/// The processor indexes the kernel has: 1, the boot processor's, until the processors are
/// discovered, and then the number every per-CPU table holds, fixed once (ADR-0008 Amendment 1,
/// D1 and D5). No count is compiled in.
static COUNT: AtomicUsize = AtomicUsize::new(1);

/// The processor indexes the kernel has: indexes `0..count()` name a slot in every per-CPU
/// table. 1 until the processors are discovered.
#[inline]
pub fn count() -> usize {
    COUNT.load(Ordering::Acquire)
}

/// Fixes [`count`] at `indexes`, once, after every per-CPU table holds that many and before any
/// application processor starts (`smp`). `false`, and nothing changed, if it was fixed already
/// or `indexes` is 0.
pub(crate) fn fix_count(indexes: usize) -> bool {
    static FIXED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if indexes == 0 || FIXED.swap(true, Ordering::AcqRel) {
        return false;
    }
    COUNT.store(indexes, Ordering::Release);
    true
}
/// IA32_GS_BASE: the base of GS in 64-bit mode.
const GS_BASE: u32 = 0xC000_0101;
/// IA32_KERNEL_GS_BASE: what `swapgs` exchanges with the base of GS.
const KERNEL_GS_BASE: u32 = 0xC000_0102;

/// A processor's dense CPU index (ADR-0008): its slot in every per-CPU array. The boot
/// processor is 0; the others are numbered in MADT order at discovery
/// (`framekernel::cpumap`). It is not the APIC ID, which IPIs and interrupt destinations
/// use ([`CpuIndex::apic_id`]): x2APIC IDs are 32-bit, sparse, and may exceed 255.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CpuIndex(usize);

impl CpuIndex {
    /// The boot processor.
    pub const BOOT: Self = Self(0);

    /// Index `index`, if the per-CPU tables have that slot (below [`count`], fixed at
    /// discovery).
    pub fn new(index: usize) -> Option<Self> {
        (index < count()).then_some(Self(index))
    }

    /// The array slot.
    pub const fn get(self) -> usize {
        self.0
    }

    /// The x2APIC ID of the processor with this index, once discovery assigned one.
    pub fn apic_id(self) -> Option<u32> {
        let id = APIC_IDS.get(self.0)?.load(Ordering::Acquire);
        (id != UNASSIGNED).then_some(id)
    }

    /// `CPU {index} (APIC ID {id})`, for reports.
    pub fn named(self) -> Named {
        Named(self)
    }
}

impl core::fmt::Display for CpuIndex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A CPU index shown with its APIC ID: `CPU 1 (APIC ID 256)`.
#[derive(Debug)]
pub struct Named(CpuIndex);

impl core::fmt::Display for Named {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0.apic_id() {
            Some(id) => write!(f, "CPU {} (APIC ID {id})", self.0),
            None => write!(f, "CPU {} (no APIC ID)", self.0),
        }
    }
}

/// What an unassigned slot of [`APIC_IDS`] holds: the broadcast ID, never one processor's.
const UNASSIGNED: u32 = crate::limits::BROADCAST;
/// Each CPU index's x2APIC ID, or [`UNASSIGNED`]. Written once per index, before that
/// processor runs any kernel code that reads it.
pub(crate) static APIC_IDS: PerCpu<AtomicU32> = PerCpu::new(
    "origin_frame::cpu::APIC_IDS",
    AtomicU32::new(UNASSIGNED),
    || AtomicU32::new(UNASSIGNED),
);

/// Records that the processor with CPU index `index` has x2APIC ID `apic_id`. The boot
/// processor records itself in [`init`]; `smp` records every other processor at discovery,
/// before it starts any (an AP checks its own entry in [`init_ap`]).
pub(crate) fn assign(index: CpuIndex, apic_id: u32) {
    if let Some(slot) = APIC_IDS.get(index.0) {
        slot.store(apic_id, Ordering::Release);
    }
}

/// One processor's CPU-local area, reached through GS. A cache line each, so processors
/// never share one.
#[repr(C, align(64))]
pub(crate) struct CpuLocal {
    /// This processor's CPU index (offset 0, read by [`this_cpu`]).
    id: AtomicU64,
    /// Address of this processor's TSS (offset 8, used by [`name_kernel_stack`]).
    tss: AtomicU64,
    /// The running context's kernel stack top (offset 16), the stack `syscall_entry`
    /// switches to; kept equal to TSS.RSP0, and both written only by [`name_kernel_stack`], from
    /// the running context's home (TASKS item 77). 0 while the boot or idle flow runs.
    kernel_stack: AtomicU64,
    /// Scratch for the user RSP during `syscall_entry` (offset 24).
    user_rsp: AtomicU64,
    /// The instruction at which this CPU's armed probe expects a fault, or 0.
    probe_rip: AtomicU64,
    /// CR2 of this CPU's last probe fault.
    probe_address: AtomicU64,
    /// Entries from ring 3 whose landing [`check_landing`] has checked on this processor (TASKS
    /// item 77). Written only by this processor, with interrupts off.
    landings: AtomicU64,
}

impl CpuLocal {
    /// An area no processor uses yet.
    pub(crate) const fn new() -> Self {
        Self {
            id: AtomicU64::new(0),
            tss: AtomicU64::new(0),
            kernel_stack: AtomicU64::new(0),
            user_rsp: AtomicU64::new(0),
            probe_rip: AtomicU64::new(0),
            probe_address: AtomicU64::new(0),
            landings: AtomicU64::new(0),
        }
    }
}

/// Each CPU index's CPU-local area. The boot processor's is in the kernel's image; the others'
/// are on the heap from discovery on, and never move, so GS's base stays valid.
pub(crate) static CPU_LOCAL: PerCpu<CpuLocal> = PerCpu::new(
    "origin_frame::cpu::CPU_LOCAL",
    CpuLocal::new(),
    CpuLocal::new,
);
/// The area of a processor that has no CPU index (its APIC ID is not the one assigned to
/// the index it was given): its index reads as `u64::MAX`, which names no array slot, so it
/// never schedules.
static OUT_OF_RANGE: CpuLocal = CpuLocal {
    id: AtomicU64::new(u64::MAX),
    tss: AtomicU64::new(0),
    kernel_stack: AtomicU64::new(0),
    user_rsp: AtomicU64::new(0),
    probe_rip: AtomicU64::new(0),
    probe_address: AtomicU64::new(0),
    landings: AtomicU64::new(0),
};

/// This processor's x2APIC ID from CPUID (leaf 0Bh EDX, else leaf 1 EBX bits 31:24), which
/// does not depend on the local APIC's mode.
pub fn cpuid_apic_id() -> u32 {
    if cpuid(0)[0] >= 0xB {
        cpuid(0xB)[3]
    } else {
        cpuid(1)[1] >> 24
    }
}

/// Points IA32_GS_BASE at the [`CpuLocal`] of CPU `index`, which records the index and this
/// processor's TSS at `tss` — if this processor's x2APIC ID (from CPUID) is the one
/// [`assign`]ed to `index`. Otherwise GS points at [`OUT_OF_RANGE`]: the processor never
/// schedules, and `smp` sees its handshake fail.
///
/// While the kernel runs GS's base is this area and IA32_KERNEL_GS_BASE holds user mode's GS
/// base (0 here, and never set by a program: FSGSBASE stays off). `swapgs` exchanges them
/// at every boundary crossing, so user mode never sees the kernel's base and nothing it does
/// to GS can reach the kernel.
fn init_cpu_local(tss: u64, index: CpuIndex) {
    let local = match CPU_LOCAL.get(index.0) {
        Some(local) if index.apic_id() == Some(cpuid_apic_id()) => {
            local.id.store(index.0 as u64, Ordering::Relaxed);
            local.tss.store(tss, Ordering::Relaxed);
            local
        }
        _ => &OUT_OF_RANGE,
    };
    // SAFETY: CPL 0 (every caller loads descriptor tables); IA32_GS_BASE exists on every
    // x86-64 processor and only sets GS's base, which only the CPU-local accessors use
    // (`this_cpu`, `local`, `syscall_entry`).
    unsafe { wrmsr(GS_BASE, ptr::from_ref(local).addr() as u64) };
    // SAFETY: CPL 0; IA32_KERNEL_GS_BASE exists on every x86-64 processor and only supplies
    // the value `swapgs` loads into GS's base. Zero: user mode starts with a null GS base.
    unsafe { wrmsr(KERNEL_GS_BASE, 0) };
    init_syscall();
}

/// CR4.MCE: a machine check raises #MC, instead of shutting the processor down.
const CR4_MCE: u64 = 1 << 6;
/// CR4.PCE: `rdpmc` at CPL 3.
const CR4_PCE: u64 = 1 << 8;
/// CR4.FSGSBASE: `rdgsbase`, `wrgsbase` and their FS forms at CPL 3.
const CR4_FSGSBASE: u64 = 1 << 16;
/// CR0.NE: x87 errors raise #MF (with it clear, the processor waits for an external FERR#
/// nothing wired up to it delivers, and the instruction hangs).
const CR0_NE: u64 = 1 << 5;
/// DR7's reset value: every breakpoint disabled.
const DR7_CLEAR: u64 = 0x400;
/// IA32_SYSENTER_CS: zero makes `sysenter` raise #GP.
const SYSENTER_CS: u32 = 0x174;

/// Whether this processor has SYSENTER's MSRs (CPUID.1:EDX.SEP).
fn has_sysenter() -> bool {
    cpuid(1)[3] & (1 << 11) != 0
}

/// Puts this processor's control state where the kernel's rules need it, whatever the firmware
/// or INIT left there: machine checks raise #MC, ring 3 cannot use `rdpmc` or the FS/GS base
/// instructions (the second is what lets [`init_cpu_local`] trust GS's base) or `sysenter`
/// (the one way into the kernel is `syscall`), and no debug breakpoint is armed. Together
/// with `enable_simd`'s CR0.NE, [`record_cpu_state`] checks all of it.
fn init_cpu_state() {
    // SAFETY: CPL 0, interrupts off (`load_tables`' contract). Setting MCE only makes machine
    // checks raise #MC, whose gate `init` installed; clearing FSGSBASE and PCE only takes
    // instructions away from CPL 3.
    unsafe {
        asm!(
            "mov {t}, cr4",
            "or {t}, {set}",
            "and {t}, {clear}",
            "mov cr4, {t}",
            set = in(reg) CR4_MCE,
            clear = in(reg) !(CR4_FSGSBASE | CR4_PCE),
            t = out(reg) _,
            options(nostack),
        );
    }
    // SAFETY: CPL 0; DR7's reset value disarms every breakpoint, and nothing here uses one. The
    // flags are not preserved: Intel leaves them undefined after a move to a debug register.
    unsafe { asm!("mov dr7, {}", in(reg) DR7_CLEAR, options(nostack)) };
    if has_sysenter() {
        // SAFETY: CPL 0; SEP says the MSR exists, and a zero selector only makes SYSENTER
        // raise #GP (Intel SDM Vol. 2B, SYSENTER).
        unsafe { wrmsr(SYSENTER_CS, 0) };
    }
}

/// CR4.
fn read_cr4() -> u64 {
    let value: u64;
    // SAFETY: reading CR4 at CPL 0 has no side effect but the one on the flags (see the module
    // documentation), which this block does not claim to preserve.
    unsafe { asm!("mov {}, cr4", out(reg) value, options(nomem, nostack)) };
    value
}

/// CR0.
fn read_cr0() -> u64 {
    let value: u64;
    // SAFETY: as `read_cr4`.
    unsafe { asm!("mov {}, cr0", out(reg) value, options(nomem, nostack)) };
    value
}

/// DR7.
fn read_dr7() -> u64 {
    let value: u64;
    // SAFETY: as `read_cr4`; Intel says the same of the debug registers.
    unsafe { asm!("mov {}, dr7", out(reg) value, options(nomem, nostack)) };
    value
}

/// Whether this processor's control state is what [`init_cpu_state`] and `enable_simd` leave.
fn cpu_state_holds() -> bool {
    let cr4 = read_cr4();
    let sysenter_cs = if has_sysenter() {
        // SAFETY: CPL 0; SEP says the MSR exists, and reading it has no side effect.
        unsafe { rdmsr(SYSENTER_CS) }
    } else {
        0
    };
    cr4 & CR4_MCE != 0
        && cr4 & (CR4_FSGSBASE | CR4_PCE) == 0
        && read_cr0() & CR0_NE != 0
        && read_dr7() & 0xFF == 0
        && sysenter_cs == 0
}

/// What each processor found about itself as it started, by CPU index: [`STATE_HOLDS`] and
/// [`FPU_ALIKE`]. Written once, by the processor itself, in one store ([`record_cpu_state`]).
pub(crate) static CPU_STATE: PerCpu<core::sync::atomic::AtomicU8> = PerCpu::new(
    "origin_frame::cpu::CPU_STATE",
    core::sync::atomic::AtomicU8::new(0),
    || core::sync::atomic::AtomicU8::new(0),
);

/// [`CPU_STATE`]'s bit: the processor passed [`cpu_state_holds`] on itself.
const STATE_HOLDS: u8 = 1 << 0;
/// [`CPU_STATE`]'s bit: its FPU format is the boot processor's, so it takes part in moves
/// (ADR-0020 S6).
const FPU_ALIKE: u8 = 1 << 1;

/// Records, in one store, whether this processor's control state holds (checked here, after
/// `enable_simd`) and whether its FPU format is the boot processor's. The boot processor and
/// every AP do it on themselves, once; an AP before its handshake, whose answer then orders
/// the record before the boot processor sees it online. `me` is the index its flow read
/// (ADR-0020 S3; TASKS item 93).
pub fn record_cpu_state(me: ThisCpu<'_>, fpu_alike: bool) {
    if let Some(state) = CPU_STATE.this(me) {
        let holds = if cpu_state_holds() { STATE_HOLDS } else { 0 };
        let alike = if fpu_alike { FPU_ALIKE } else { 0 };
        state.store(holds | alike, Ordering::Release);
    }
}

/// How many of `cpus` recorded the bit `bit` of [`CPU_STATE`].
fn recorded(cpus: &[CpuIndex], bit: u8) -> usize {
    cpus.iter()
        .filter(|c| {
            CPU_STATE
                .get(c.get())
                .is_some_and(|s| s.load(Ordering::Acquire) & bit != 0)
        })
        .count()
}

/// How many of `cpus` recorded a control state that holds.
pub fn cpu_states_holding(cpus: &[CpuIndex]) -> usize {
    recorded(cpus, STATE_HOLDS)
}

/// How many of `cpus` recorded an FPU format alike the boot processor's.
pub fn fpu_formats_alike(cpus: &[CpuIndex]) -> usize {
    recorded(cpus, FPU_ALIKE)
}

/// Whether processor `index` recorded an FPU format alike the boot processor's (ADR-0020 S6,
/// N9). `false` for a processor that recorded nothing: an AP records only once every check
/// before its handshake has passed, so one refused for any reason never says alike.
///
/// Not the whole answer to whether it takes part in moves: an AP whose answer came after the
/// boot processor stopped waiting is parked with its record standing, and nothing writes it
/// again. So the question is asked through the processors online,
/// `smp::Online::takes_part_in_moves` (the review of TASKS item 94, R94-3).
pub(crate) fn fpu_alike(index: CpuIndex) -> bool {
    recorded(&[index], FPU_ALIKE) == 1
}

/// IA32_STAR, IA32_LSTAR and IA32_FMASK.
const STAR: u32 = 0xC000_0081;
const LSTAR: u32 = 0xC000_0082;
const FMASK: u32 = 0xC000_0084;
/// RFLAGS bits SYSCALL clears: TF, IF, DF, NT and AC. With IF clear, no interrupt can
/// arrive before `syscall_entry` has left the user stack.
const SYSCALL_FMASK: u64 = 0x4_4700;

/// Enables SYSCALL (EFER.SCE) with `syscall_entry` as its target. STAR's kernel base 0x08
/// gives CS 0x08 and SS 0x10 on entry; its user base 0x20 would give SYSRET CS 0x33 and
/// SS 0x2B, the user segments, although the kernel returns with IRETQ for now.
fn init_syscall() {
    let star = (0x20_u64 << 48) | (u64::from(KERNEL_CODE) << 32);
    // SAFETY: CPL 0; these MSRs exist on every x86-64 processor; the values configure
    // only the SYSCALL instruction, whose target is the kernel's own entry stub.
    let efer = unsafe { rdmsr(EFER) };
    // SAFETY: as above; EFER.SCE (bit 0) only enables SYSCALL/SYSRET.
    unsafe { wrmsr(EFER, efer | 1) };
    // SAFETY: as above.
    unsafe { wrmsr(STAR, star) };
    // SAFETY: as above.
    unsafe { wrmsr(LSTAR, (syscall_entry as *const ()).addr() as u64) };
    // SAFETY: as above.
    unsafe { wrmsr(FMASK, SYSCALL_FMASK) };
}

/// The SYSCALL target, the only way into the kernel from ring 3 other than a fault or an
/// interrupt (ADR-0005 P4): swaps the kernel's GS base in, switches from the user stack to
/// the running thread's kernel stack (from the CPU-local area; interrupts are off, SFMASK
/// cleared IF), builds the frame an interrupt from user mode would have pushed (SS, RSP,
/// RFLAGS from R11, CS, RIP from RCX) with vector [`SYSCALL_VECTOR`], and joins the common
/// path after its `swapgs`, returning with IRETQ.
#[unsafe(naked)]
extern "sysv64" fn syscall_entry() {
    naked_asm!(
        "swapgs",
        "mov gs:[24], rsp",
        "mov rsp, gs:[16]",
        "push {ss}",
        "push qword ptr gs:[24]",
        "push r11",
        "push {cs}",
        "push rcx",
        "push 0",
        "push {vector}",
        "jmp {saved}",
        ss = const d::USER_DATA,
        cs = const d::USER_CODE,
        vector = const SYSCALL_VECTOR,
        saved = sym exception_saved,
    );
}

/// This processor's CPU-local area, lent for the life of the witness `here` (ADR-0020 S3; TASKS
/// item 93). The index is read inside this function under that witness, not by its caller, as
/// before TASKS item 93, so `dispatch`'s code stays the base's (TASKS item 93's static counts of
/// 2026-11-21 found the read moved into the caller).
fn local(here: crate::trap::Here<'_>) -> &'_ CpuLocal {
    CPU_LOCAL.this(this_cpu(here)).unwrap_or(&OUT_OF_RANGE)
}

/// Names the stack the running context's interrupts and system calls from ring 3 land on: this
/// processor's kernel-stack record and its TSS.RSP0 become the top of the slot holding `home`, the
/// incoming context's home in the table of contexts, or 0 for the boot or idle flow, whose home
/// is `BOOT_FLOW` and which never runs in ring 3 (TASKS item 77). Only [`resume`] calls it, with
/// the home its table's switch answered, so the scheduler above cannot name another context's
/// stack, nor a stack that carries another context's suspended frame. The stack switch this
/// sets up is the one the Intel SDM describes for a privilege change in IA-32e mode (Vol. 3A,
/// order 325384-093US, September 2026, read 2026-11-20: §7.14.4, "Stack Switching in IA-32e
/// Mode"; §10.7, the 64-bit TSS's RSPn); SYSCALL loads no stack pointer of its own (§6.8.8), so
/// `syscall_entry` loads the record. A home in no kernel stack cannot be one a published context
/// has, and halts the processor rather than name nothing for a context that may enter ring 3. The
/// boot or idle flow's 0 names no stack: were it ever used, the processor's pushes would wrap to
/// the top page of the kernel-image window, which nothing maps: only the image is mapped there,
/// placed at least 2 MiB below the window's top (`layout::place`), and its guard pages are
/// unmapped ones. So the delivery would fault into #DF, on its own stack, rather than write
/// anywhere (the review of TASKS items 75, 77 and 81, R77-3).
///
/// `at` is the trap entry's witness ([`resume`]'s), under which the index is read (ADR-0020 S3;
/// TASKS item 93).
fn name_kernel_stack(at: crate::trap::Here<'_>, home: u64) {
    let top = if home == BOOT_FLOW {
        0
    } else {
        match crate::stack::slot_top(home) {
            Some(top) => top,
            None => panic!(
                "the context resumed {} has its home in no kernel stack",
                this_cpu(at).index().named()
            ),
        }
    };
    let local = this_cpu(at);
    let Some(tss) = CPU_LOCAL.this(local).map(|l| l.tss.load(Ordering::Relaxed)) else {
        return;
    };
    if tss == 0 {
        return;
    }
    let field = ptr::with_exposed_provenance_mut::<u32>(tss as usize + d::TSS_RSP0_OFFSET);
    if let Some(l) = CPU_LOCAL.this(local) {
        // Release, read with Acquire by `stack_in_use` (TASKS item 70, Q2).
        l.kernel_stack.store(top, Ordering::Release);
    }
    // SAFETY: `tss` is this processor's TSS (recorded by `init_cpu_local`), live for ever
    // and written only by this processor; RSP0 is two `u32` words at byte 4, 4-aligned.
    unsafe { field.write_volatile(top as u32) };
    // SAFETY: as above, the high word.
    unsafe { field.wrapping_add(1).write_volatile((top >> 32) as u32) };
}

/// The IST slot vector `vector`'s gate switches to (0: none), as [`init`] installs them: an entry
/// through one of these lands on that stack of the interrupt stack table, never on the running
/// context's (Intel SDM Vol. 3A, order 325384-093US, §7.14.5, "Interrupt Stack Table").
const fn ist_of(vector: u64) -> u8 {
    match vector {
        1 => DEBUG_IST,
        2 => NMI_IST,
        8 => DOUBLE_FAULT_IST,
        18 => MACHINE_CHECK_IST,
        _ => 0,
    }
}

/// Checks that an entry from ring 3 at frame `here`, through a vector with no IST stack, landed
/// in the running context's own stack (TASKS item 77, I2), and counts it. The source is the table
/// of contexts: the home of its `current` slot, read lock-free with interrupts off, as the trap
/// entry runs, and written only by this processor's switches. Never this processor's
/// kernel-stack record or TSS.RSP0, which [`name_kernel_stack`] wrote from the same table: a
/// wrong record would land the entry on the wrong stack and pass a check against itself (the
/// coordinating session's addition (c)). A system call enters here too: `syscall_entry` builds
/// an interrupt's frame on the stack its record names and joins the common path, so this check,
/// at the start of [`dispatch`], is also the first thing the system-call path does in Rust (the
/// addition (d)). A frame elsewhere was pushed onto another stack, perhaps over a suspended
/// context's frame: the processor halts, as the trap entry does for a frame it never saved.
///
/// `at` is the trap entry's witness, under which the index is read (ADR-0020 S3; TASKS item 93).
/// The count's plain load and store rest on more than any `Here` says: that interrupts are off.
/// A `Here` from [`crate::trap::Pinned::here`] holds with interrupts on, and an interrupt between
/// the load and the store would lose its own count (review finding R93-10). So this stays
/// private, and [`dispatch`], which runs with interrupts off, is its one caller; an interrupts-off
/// token of its own is TASKS item 96's (R95-12).
fn check_landing(at: crate::trap::Here<'_>, here: u64, vector: u64) {
    let me = this_cpu(at);
    let home = CONTEXTS.this(me).and_then(Contexts::running_home);
    if !home.is_some_and(|home| crate::stack::same_slot(home, here)) {
        panic!(
            "an entry from ring 3 (vector {vector}) {} landed on a stack not the running context's own",
            me.index().named()
        );
    }
    if let Some(l) = CPU_LOCAL.this(me) {
        // Only this processor writes it, with interrupts off: a plain load and store.
        l.landings.store(
            l.landings.load(Ordering::Relaxed).wrapping_add(1),
            Ordering::Relaxed,
        );
    }
}

/// How many entries from ring 3 every processor's [`check_landing`] has checked: for the boot
/// proof, which shows that the check ran.
pub fn landings_checked() -> u64 {
    CPU_LOCAL
        .iter()
        .map(|l| l.landings.load(Ordering::Relaxed))
        .fold(0, u64::wrapping_add)
}

/// Each processor's contexts (TASKS item 70): for each thread published to it, and its boot or
/// idle flow, the context's own stack (its home) and the frame the trap entry may resume. Only
/// a frame recorded here is ever resumed, so a wrong answer from the scheduler above cannot make
/// the trap entry load anything else. A context's home stays recorded from its publication until
/// its processor has left its stack after it ended, and no switch writes it, so a stack a
/// context may still use is always in some processor's table for any processor's `stack_in_use`:
/// in this one, or, while it moves (ADR-0020 S4), in its destination's before it leaves this one,
/// which the scans through [`MOVES`] cannot miss.
///
/// It replaces `SAVED_FRAMES`, whose 32 slots held only suspended threads' frames. A slot is now
/// held from a thread's publication until it has ended and its processor has left its stack, so
/// the capacity is counted again (the coordinating session's ruling (a) on TASKS item 70): a
/// processor has at most one context per slot of its run queue, whose 16
/// (`runqueue::CAPACITY`) include the boot or idle flow's, plus at most one that has ended and is
/// not yet retired, 17 of `origin_sync::contexts::SLOTS`' 32, and, since TASKS item 75, each
/// spawn to it between its publication and its queueing (`origin_sync::contexts`, "Capacity").
/// The kernel checks the 17 at compile time (`sched.rs`). A full table refuses a publication
/// ([`publish_context`], so the kernel refuses the spawn), and refuses a boot or idle flow's first
/// switch-out, which halts.
pub(crate) static CONTEXTS: PerCpu<Contexts> = PerCpu::new(
    "origin_frame::cpu::CONTEXTS",
    Contexts::new(),
    Contexts::new,
);

/// The machine's move sequence (ADR-0020 S4; `origin_sync::contexts`, "Moves between
/// processors"): the move gate and the begun and finished counters. A move of a context between
/// two processors' tables of contexts holds the gate, and every scan of all the tables goes
/// through [`Moves::scan`], so that a move cannot hide a home from it. Nothing moves a context
/// yet: TASKS item 96 makes the first move.
pub(crate) static MOVES: Moves = Moves::new();

/// What a scan's pass under the move gate runs in: interrupts off on this processor, since a
/// mover holds the gate with interrupts off and may be this processor's own interrupt handler.
struct Quietly;

impl Quiet for Quietly {
    fn quietly<R>(&self, f: impl FnOnce() -> R) -> R {
        without_interrupts(f)
    }
}

/// Records a new thread on processor `cpu`, whose first frame is `frame`, on its own stack: the
/// frame becomes its home and the frame to resume. `false` if the table is full or `cpu` has
/// none.
pub(crate) fn publish_context(cpu: CpuIndex, frame: u64) -> bool {
    CONTEXTS
        .get(cpu.get())
        .is_some_and(|table| table.publish(frame))
}

/// Whether some processor's table of contexts covers `[start, end)`: a context's home, saved
/// frame or leaving frame there. Part of what [`stack_in_use`] asks, without the caller's stack
/// pointer and the kernel-stack records (`stack::slot_in_contexts`). It scans every table through
/// the move sequence ([`MOVES`]), so a context moving between two tables is not missed: at most
/// four passes with no lock, then one under the move gate with interrupts off (ADR-0020 S4).
pub(crate) fn contexts_cover(start: u64, end: u64) -> bool {
    MOVES
        .scan(
            || CONTEXTS.iter().any(|table| table.covers(start, end)),
            &Quietly,
        )
        .covered
}

/// Withdraws the context published on processor `cpu` at `frame`, which its spawner never made
/// runnable (TASKS item 75; `Contexts::withdraw`). `false` if the table does not hold it as never
/// resumed, or `cpu` has none.
pub(crate) fn withdraw_context(cpu: CpuIndex, frame: u64) -> bool {
    CONTEXTS
        .get(cpu.get())
        .is_some_and(|table| table.withdraw(frame))
}

/// This processor's stack pointer.
fn stack_pointer() -> u64 {
    let rsp: u64;
    // SAFETY: reads the stack pointer; no memory, no stack, flags preserved.
    unsafe { asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags)) };
    rsp
}

/// Whether this processor runs on the kernel stack holding `address`: the stack pointer is in
/// the same slot of the stack region. The check a retirement makes (`Contexts::retire_ended`).
fn running_on(address: u64) -> bool {
    let slot = crate::stack::slot_holding(address);
    slot.is_some() && slot == crate::stack::slot_holding(stack_pointer())
}

/// What the trap entry resumes after a scheduler upcall: `next`, the frame the scheduler
/// answered for the interrupted context at `here` (`Contexts::switch`). Unless they are the same,
/// `next` must be a frame this processor saved, and the context at `here` leaves `here` as its
/// frame, unless it is ending (`exiting`). The table publishes that frame only at this
/// processor's next call into it, the next switch or a stack's drop: [`exception_saved`] still
/// returns along the outgoing stack after this, until its `mov rsp, rax`, so the frame must not
/// be resumable anywhere before then (ADR-0020 S5). No address is printed (ADR-0004 Amendment 2,
/// A6.2): a frame is told as an offset in its window.
///
/// `at` is the trap entry's witness, under which the index is read (ADR-0020 S3; TASKS item 93).
fn resume(at: crate::trap::Here<'_>, here: u64, next: u64, exiting: bool) -> u64 {
    let me = this_cpu(at);
    let outcome = CONTEXTS.this(me).map_or(Err(Refused::NeverSaved), |table| {
        table.switch(here, next, exiting, running_on)
    });
    match outcome {
        Ok(Switched { frame, home }) => {
            // The incoming context's own stack, from its home, unless nothing switched.
            if let Some(home) = home {
                name_kernel_stack(at, home);
            }
            frame
        }
        Err(Refused::NeverSaved) => panic!(
            "the scheduler returned a frame {} never saved: {}",
            me.index().named(),
            crate::phys::told(next)
        ),
        Err(Refused::Full) => panic!("too many saved thread frames on this processor"),
        Err(Refused::StillOnIt) => {
            panic!("an ended thread's stack was to be retired while this processor still ran on it")
        }
    }
}

/// Whether a processor may be using the kernel stack `[start, end)`, whose top is `top`: the
/// caller's own stack pointer is on it, a processor's kernel-stack record names it, or a
/// context's home, saved frame or leaving frame is on it (TASKS item 70; ADR-0020 S5). Only a
/// stack in none of them can be freed. It takes no lock but the move gate, and that only in a
/// scan's last pass, after its fast passes each overlapped a move (ADR-0020 S4; [`MOVES`]): so a
/// stack may be dropped under any lock but the gate, and a mover holding the gate must drop none.
///
/// It first publishes the frame this processor's last switch-out left, and retires its own ended
/// context, if any: this processor has left both stacks, since any code it runs after a switch
/// runs on another context's stack (`Contexts::publish_leaving`, `Contexts::retire_ended`; ADR-0020
/// S5). So a stack reaped here before this processor's next switch is given back, not leaked.
/// Both run with interrupts off, as the table asks: a switch that interrupted the retirement
/// between its take and its clear would leave the ended stack in no record that retires it, and a
/// reap then would leak it (the review of TASKS item 70, T70-1); one that interrupted the
/// publication between its two stores would have its own leaving record cleared. The processor's
/// index is read inside that section too, under its witness, so the table is this processor's
/// own even once a thread can be moved between a read and its use (the review of TASKS item 95,
/// R95-3; ADR-0020 S3, TASKS item 93).
pub(crate) fn stack_in_use(start: u64, end: u64, top: u64) -> bool {
    let used = |value: u64| value >= start && value < end;
    if tables_ready()
        && without_interrupts_here(|at| {
            CONTEXTS.this(this_cpu(at)).map(|table| {
                table.publish_leaving();
                table.retire_ended(running_on)
            })
        }) == Some(Err(Refused::StillOnIt))
    {
        panic!("an ended thread's stack was to be retired while this processor still ran on it");
    }
    used(stack_pointer())
        // `kernel_stack` holds the top of the running context's own stack: this stack's top is
        // one past its end, so compare it exactly (another stack's top can equal our guard
        // page's address). Acquire, with `name_kernel_stack`'s Release. Since TASKS item 77 it
        // names only a stack whose home is in the table, so it adds no case the table misses;
        // it stays, as the record `syscall_entry` reads (the coordinating session's Q6).
        || CPU_LOCAL
            .iter()
            .any(|l| l.kernel_stack.load(Ordering::Acquire) == top)
        // Last, through the move sequence: neither of the records above can name a context that
        // is moving, which is suspended and not running.
        || contexts_cover(start, end)
}

/// This processor's CPU index: one GS-relative load from its CPU-local area. On a
/// processor without one ([`OUT_OF_RANGE`]) it is an index no array has.
///
/// Private (ADR-0020 S3; TASKS item 93): [`this_cpu`] is its one caller, under a witness that the
/// reader cannot leave the processor. A read without one does not build.
///
/// The witness does not show that GS's base is set (review finding R93-3): a section's witness,
/// and the boot flow's, exist before `load_tables` runs. What does is the boot's order. On the
/// boot processor every step that reads the index comes after `Early::load_cpu_tables` in
/// `Early`'s order or checks [`tables_ready`] first (`timer::init`); on an AP, `ap_entry` reads it
/// only after `init_ap`.
fn read_index() -> CpuIndex {
    let id: u64;
    // SAFETY: `load_tables` set GS's base to a live `CpuLocal` on every processor before any code
    // that calls this runs (the boot's order, above, not the witness), and offset 0 is its `id`,
    // a `u64`.
    unsafe { asm!("mov {}, gs:[0]", out(reg) id, options(nostack, readonly, preserves_flags)) };
    CpuIndex(usize::try_from(id).unwrap_or(usize::MAX))
}

/// Software interrupt a kernel thread raises to yield the processor.
pub const YIELD_VECTOR: u8 = 0x81;
/// Software interrupt to sleep for RDI timer ticks.
pub const SLEEP_VECTOR: u8 = 0x82;
/// Software interrupt to end the running thread.
pub const EXIT_VECTOR: u8 = 0x83;
/// Software interrupt: the running kernel thread blocks until woken (`sched::block`).
pub const BLOCK_VECTOR: u8 = 0x84;
/// IPI: a thread queued on this processor was woken by another (`sched::wake`).
pub const RESCHEDULE_VECTOR: u8 = 0xF1;
/// The NVMe controller's I/O completion interrupt (an MSI-X message).
pub const DEVICE_VECTOR: u8 = 0x40;
/// A legacy IRQ routed through the I/O APIC (the PIT, in the routing proof).
pub const IOAPIC_VECTOR: u8 = 0x41;
/// The virtio-net NIC's receive used-buffer interrupt (an MSI-X message).
pub const NET_VECTOR: u8 = 0x42;
/// The virtio-net NIC's transmit-completion interrupt (an MSI-X message).
pub const NET_TX_VECTOR: u8 = 0x43;

/// The vector in a system call's frame (no gate has it: `syscall` builds the frame itself,
/// and `int 0x80` from ring 3 faults).
pub const SYSCALL_VECTOR: u8 = 0x80;

/// Local APIC timer interrupts taken since boot.
static TICKS: AtomicU64 = AtomicU64::new(0);
/// NMIs taken, and the frame address of the last one.
static NMIS: AtomicU64 = AtomicU64::new(0);
static NMI_FRAME: AtomicU64 = AtomicU64::new(0);

/// NMIs taken so far and the frame address of the last.
pub fn nmis() -> (u64, u64) {
    (
        NMIS.load(Ordering::SeqCst),
        NMI_FRAME.load(Ordering::SeqCst),
    )
}

/// The boot processor's NMI stack, as `[low, high)`.
pub fn bsp_nmi_stack() -> (u64, u64) {
    let low = bsp_stacks_at(core::mem::offset_of!(BspStacks, nmi.stack));
    (low, low + IST_STACK_SIZE as u64)
}

/// The tops of processor `cpu`'s interrupt stacks as its TSS holds them: #DF, NMI, #MC and #DB
/// (IST1–IST4). `None` before that processor loaded its tables. For the proof that every
/// application processor's interrupt stacks are in the stack region (ADR-0004 Amendment 1, A2).
pub fn interrupt_stack_tops(cpu: CpuIndex) -> Option<[u64; 4]> {
    let tss = CPU_LOCAL.get(cpu.0)?.tss.load(Ordering::Relaxed);
    if tss == 0 {
        return None;
    }
    // The two words of the TSS image from word `at`, low first.
    let ist = |at: usize| {
        let p = ptr::with_exposed_provenance::<[u32; 2]>(tss as usize + 4 * at);
        // SAFETY: `tss` is a processor's TSS image (`init_cpu_local` recorded it), 26 words that
        // live for ever (a static, or tables `smp` never frees). IST1-IST4 are written once,
        // before `ltr`, and never again, so nothing writes these words while they are read; the
        // image is 4-aligned, as `[u32; 2]` needs; and only these words are read, with no
        // reference made to the TSS, whose RSP0 its processor writes.
        let [low, high] = unsafe { p.read_volatile() };
        u64::from(low) | u64::from(high) << 32
    };
    Some([ist(9), ist(11), ist(13), ist(TSS_IST4_WORD)])
}

/// The frame of the last user-mode #DB, for the boot proof that it arrives on its own stack.
static DEBUG_FRAME: AtomicU64 = AtomicU64::new(0);

/// Where the last #DB in user mode saved its frame.
pub fn last_debug_frame() -> u64 {
    DEBUG_FRAME.load(Ordering::SeqCst)
}

/// The boot processor's #DB stack, as `[low, high)`.
pub fn bsp_debug_stack() -> (u64, u64) {
    let low = bsp_stacks_at(core::mem::offset_of!(BspStacks, debug.stack));
    (low, low + IST_STACK_SIZE as u64)
}

/// Disables interrupts and halts this processor forever.
pub fn halt() -> ! {
    loop {
        // SAFETY: the kernel runs at CPL 0. `cli; hlt` touches no memory and needs no stack.
        // An NMI or SMI can still resume the processor, so it halts again.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

/// Gives up the processor to the next ready context (`sched::yield_now`).
pub fn thread_yield() {
    // SAFETY: the gate for this vector is installed; the handler saves and restores every
    // register, and returns here when this context is next scheduled.
    unsafe { asm!("int 0x81") };
}

/// Sleeps for `ticks` timer ticks of this processor.
pub fn thread_sleep(ticks: u64) {
    // SAFETY: as for `thread_yield`; RDI carries the argument and is restored.
    unsafe { asm!("int 0x82", in("rdi") ticks) };
}

/// Ends the running thread: never returns.
pub fn thread_exit() -> ! {
    loop {
        // SAFETY: as for `thread_yield`; the handler frees this thread's slot and never
        // resumes it. If the request is refused (no active queue), it is retried.
        unsafe { asm!("int 0x83") };
    }
}

/// Blocks the running thread until it is woken, or for `ticks` timer ticks if non-zero.
pub fn thread_block(ticks: u64) {
    // SAFETY: as for `thread_yield`; the handler saves and restores every register, and
    // this context resumes here once it is woken. RDI carries the tick limit.
    unsafe { asm!("int 0x84", in("rdi") ticks) };
}

/// Halts with interrupts enabled until the next interrupt, then returns with interrupts enabled.
/// The idle context of a processor.
pub fn idle() {
    if tables_ready() {
        // SAFETY: CPL 0; every vector that can arrive has a gate (`tables_ready`).
        unsafe { asm!("sti", "hlt", options(nostack)) };
    }
}

/// As [`idle`], returning with interrupts disabled again.
pub fn idle_disabling() {
    if tables_ready() {
        // SAFETY: as for `idle`; CLI after the halt.
        unsafe { asm!("sti", "hlt", "cli", options(nostack)) };
    }
}

/// Loads `values` into XMM0, XMM3, XMM7 and XMM15 (the SIMD state the boot proof checks).
/// Touches only those registers: kernel code is soft-float, so nothing else uses them.
pub fn xmm_load(values: [u64; 4]) {
    // SAFETY: SSE is enabled on this processor (`enable_simd`); these instructions only load
    // the named XMM registers from general registers. No `preserves_flags`: `movq` is an SSE
    // instruction, and none is on the list of the module documentation.
    unsafe {
        asm!(
            "movq xmm0, {0}",
            "movq xmm3, {1}",
            "movq xmm7, {2}",
            "movq xmm15, {3}",
            in(reg) values[0], in(reg) values[1], in(reg) values[2], in(reg) values[3],
            out("xmm0") _, out("xmm3") _, out("xmm7") _, out("xmm15") _,
            options(nomem, nostack),
        );
    }
}

/// Reads XMM0, XMM3, XMM7 and XMM15 (the SIMD state the boot proof checks).
pub fn xmm_read() -> [u64; 4] {
    let (a, b, c, d): (u64, u64, u64, u64);
    // SAFETY: reads the named XMM registers; no memory, no stack. No `preserves_flags`, as in
    // `xmm_load`.
    unsafe {
        asm!(
            "movq {0}, xmm0",
            "movq {1}, xmm3",
            "movq {2}, xmm7",
            "movq {3}, xmm15",
            out(reg) a, out(reg) b, out(reg) c, out(reg) d,
            options(nomem, nostack),
        );
    }
    [a, b, c, d]
}

/// Local APIC timer interrupts taken since boot.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Executes `int3`; the breakpoint handler reports it and returns here.
pub fn breakpoint() {
    if tables_ready() {
        // SAFETY: `int3` raises #BP, whose gate `init` installed (`tables_ready`); the handler
        // returns with `iretq` to the instruction after `int3`, preserving every register.
        unsafe { asm!("int3") };
    }
}

/// Called by [`exception_common`] with the saved frame.
///
/// Everything above the framework is reached through [`crate::trap::hooks`] and
/// [`origin_frame::irq`]: this function does not know the scheduler, the system-call layer,
/// the console or any driver.
extern "sysv64" fn dispatch(frame: &mut ExceptionFrame) -> u64 {
    use crate::irq::{self, Action};
    // The frame to resume: this one, unless the scheduler switches to another thread.
    let here = ptr::from_mut(frame).addr() as u64;
    // An entry from ring 3 must have landed on the running context's own stack (TASKS item 77).
    if frame.cs & 3 == 3 && ist_of(frame.vector) == 0 {
        check_landing(Switch::new().here(), here, frame.vector);
    }
    // A program can set RFLAGS.AC; the entry path must have cleared it for the kernel.
    if frame.cs & 3 == 3 && SMAP_ON.load(Ordering::Relaxed) && read_rflags() & RFLAGS_AC != 0 {
        AC_LEAKS.fetch_add(1, Ordering::Relaxed);
    }
    let hooks = crate::trap::hooks();
    // Device interrupts: the timer counts and acknowledges; a spurious interrupt needs no
    // EOI (SDM Vol. 3A, spurious interrupt).
    if frame.vector == u64::from(TIMER_VECTOR) {
        TICKS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: x2APIC mode is enabled before the timer is unmasked (`timer::init`), and
        // writing 0 to the EOI register only acknowledges the interrupt in service.
        unsafe { wrmsr(X2APIC_EOI, 0) };
        return hooks.map_or(here, |h| {
            resume(
                Switch::new().here(),
                here,
                (h.tick)(here, &Switch::new()),
                false,
            )
        });
    }
    if frame.vector == u64::from(SPURIOUS_VECTOR) {
        return here;
    }
    // A device's interrupt message, or a legacy IRQ routed through the I/O APIC: acknowledge,
    // run the handler its driver registered, and let the scheduler switch to a thread it woke.
    if let Ok(vector) = u8::try_from(frame.vector)
        && irq::is_device_vector(vector)
    {
        // SAFETY: as for the timer: interrupt messages are only routed to processors whose
        // APIC is in x2APIC mode; the write only acknowledges the interrupt.
        unsafe { wrmsr(X2APIC_EOI, 0) };
        return match irq::run(vector, Switch::new().here()) {
            Action::Resume => here,
            Action::Reschedule => hooks.map_or(here, |h| {
                resume(
                    Switch::new().here(),
                    here,
                    (h.reschedule)(here, &Switch::new()),
                    false,
                )
            }),
        };
    }
    // Another processor woke a thread queued here: acknowledge, then switch to it if it
    // should run now, rather than at the next timer tick.
    if frame.vector == u64::from(RESCHEDULE_VECTOR) {
        // SAFETY: as for the timer: only processors with an active scheduler are sent this
        // vector, so x2APIC mode is on; the write only acknowledges the interrupt.
        unsafe { wrmsr(X2APIC_EOI, 0) };
        return hooks.map_or(here, |h| {
            resume(
                Switch::new().here(),
                here,
                (h.reschedule)(here, &Switch::new()),
                false,
            )
        });
    }
    // Another processor changed a kernel mapping: invalidate it here, then acknowledge.
    if frame.vector == u64::from(crate::tlb::SHOOTDOWN_VECTOR) {
        crate::tlb::interrupted(Switch::new().here());
        // SAFETY: as for the timer: x2APIC mode is on (only processors with an active
        // scheduler are sent this vector), and the write only acknowledges the interrupt.
        unsafe { wrmsr(X2APIC_EOI, 0) };
        return here;
    }
    // NMI: counted and resumed (no NMI source is used yet besides the boot proof).
    if frame.vector == 2 {
        NMI_FRAME.store(here, Ordering::SeqCst);
        NMIS.fetch_add(1, Ordering::SeqCst);
        return here;
    }
    // Scheduler requests from kernel threads (`sched::yield_now`, `sleep`, `exit`).
    if frame.vector == u64::from(YIELD_VECTOR) {
        return hooks.map_or(here, |h| {
            resume(
                Switch::new().here(),
                here,
                (h.yield_now)(here, &Switch::new()),
                false,
            )
        });
    }
    if frame.vector == u64::from(SLEEP_VECTOR) {
        return hooks.map_or(here, |h| {
            resume(
                Switch::new().here(),
                here,
                (h.sleep)(here, frame.rdi, &Switch::new()),
                false,
            )
        });
    }
    if frame.vector == u64::from(EXIT_VECTOR) {
        return hooks.map_or(here, |h| {
            resume(
                Switch::new().here(),
                here,
                (h.exit)(here, &Switch::new()),
                true,
            )
        });
    }
    if frame.vector == u64::from(BLOCK_VECTOR) {
        return hooks.map_or(here, |h| {
            resume(
                Switch::new().here(),
                here,
                (h.block)(here, frame.rdi, &Switch::new()),
                false,
            )
        });
    }
    if frame.vector == u64::from(SYSCALL_VECTOR) {
        let args = [
            frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
        ];
        let Some(hooks) = hooks else {
            halt();
        };
        return match (hooks.syscall)(frame.rax, args, frame.rip) {
            crate::trap::SyscallOutcome::Status(status) => {
                frame.rax = status as u64;
                here
            }
            crate::trap::SyscallOutcome::ThreadExit => resume(
                Switch::new().here(),
                here,
                (hooks.exit)(here, &Switch::new()),
                true,
            ),
        };
    }
    // A machine check is the hardware reporting on itself: it says nothing about the program
    // it interrupted, and it can only be recovered from or stop the processor.
    let machine_check_report = if frame.vector == 18 {
        match machine_check() {
            Ok(()) => return here,
            Err(report) => Some(report),
        }
    } else {
        None
    };
    // An exception in user mode ends that thread; the kernel carries on (crash isolation).
    // Neither #DF (its saved CS is architecturally undefined) nor #MC (taken above) says
    // anything about user mode. CR2 means something only for a page fault: read for any
    // other vector, it is the address of the last page fault on this processor, which may
    // be another process's.
    if frame.vector < 32 && !matches!(frame.vector, 8 | 18) && frame.cs & 3 == 3 {
        let address = if frame.vector == 14 { read_cr2() } else { 0 };
        // #DB is the one vector here with a stack of its own, which takes no interrupts.
        let on_exception_stack = frame.vector == 1;
        if on_exception_stack {
            DEBUG_FRAME.store(here, Ordering::SeqCst);
        }
        let Some(hooks) = hooks else {
            halt();
        };
        (hooks.user_fault)(frame.vector, frame.error_code, address, on_exception_stack);
        return resume(
            Switch::new().here(),
            here,
            (hooks.exit)(here, &Switch::new()),
            true,
        );
    }
    // A page fault at the user-copy instruction, on a user address: resume at
    // `probe_recover`, which returns "failed" to `copy_user`'s caller.
    if frame.vector == 14
        && frame.cs & 3 == 0
        && frame.rip == (user_copy as *const ()).addr() as u64
        && crate::usercopy::is_user_address(read_cr2())
    {
        USER_COPY_FAULTS.fetch_add(1, Ordering::Relaxed);
        frame.rax = 0;
        frame.rip = (probe_recover as *const ()).addr() as u64;
        return here;
    }
    // A page fault at the exact instruction a probe armed: record it and resume at
    // `probe_recover`, which returns the error code to the probe's caller.
    if frame.vector == 14 {
        // Two reads of the index, each inside `local` under the trap entry's witness: as the code
        // read it before TASKS item 93, so what is built here does not change.
        let armed = local(Switch::new().here()).probe_rip.load(Ordering::SeqCst);
        if armed != 0 && frame.rip == armed {
            local(Switch::new().here())
                .probe_address
                .store(read_cr2(), Ordering::SeqCst);
            frame.rax = frame.error_code;
            frame.rip = (probe_recover as *const ()).addr() as u64;
            return here;
        }
    }
    // A report must get out even if this processor faulted inside the console (the hook,
    // `console::report_exception`, uses the console's urgent path). Only #BP resumes.
    let vector = frame.vector as u8;
    if let Some(hooks) = hooks {
        let _ = (
            frame.rsi, frame.rdi, frame.rbp, frame.r8, frame.r9, frame.r10, frame.r11,
        );
        let _ = (frame.r12, frame.r13, frame.r14, frame.r15, frame.ss);
        (hooks.report_exception)(&crate::trap::ExceptionReport {
            vector,
            error_code: frame.error_code,
            rip: frame.rip,
            rsp: frame.rsp,
            rflags: frame.rflags,
            cs: frame.cs,
            rax: frame.rax,
            rbx: frame.rbx,
            rcx: frame.rcx,
            rdx: frame.rdx,
            machine_check: machine_check_report
                .as_ref()
                .map(|report| report as &dyn core::fmt::Display),
        });
    }
    if vector == 3 {
        return here;
    }
    halt()
}

/// The machine-check MSRs and bits (Intel SDM Vol. 3B §16.3): CPUID.1:EDX.MCA, the number of
/// banks in IA32_MCG_CAP, restart IP valid and machine check in progress in IA32_MCG_STATUS,
/// and valid, uncorrected and processor context corrupt in each bank's IA32_MCi_STATUS.
const CPUID_MCA: u32 = 1 << 14;
const MCG_CAP: u32 = 0x179;
const MCG_STATUS: u32 = 0x17A;
const MCG_RIPV: u64 = 1 << 0;
const MCG_MCIP: u64 = 1 << 2;
const MC0_STATUS: u32 = 0x401;
const MC_VAL: u64 = 1 << 63;
const MC_UC: u64 = 1 << 61;
const MC_PCC: u64 = 1 << 57;
/// The most banks read (MCG_CAP's count, whatever it says, indexes at most this many).
const MC_BANKS: u64 = 32;

/// Why a machine check cannot be recovered from.
struct FatalMachineCheck {
    /// IA32_MCG_STATUS (0 if the processor has no machine-check MSRs).
    status: u64,
    /// The first bank reporting an uncorrected error or a corrupt processor context.
    bank: Option<(u64, u64)>,
}

impl core::fmt::Display for FatalMachineCheck {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "IA32_MCG_STATUS {:#x}", self.status)?;
        if let Some((bank, status)) = self.bank {
            write!(f, ", bank {bank} IA32_MCi_STATUS {status:#x}")?;
        }
        write!(f, ", not recoverable")
    }
}

/// Reads the machine-check state and decides: recoverable means the processor can restart the
/// interrupted instruction (RIPV) and no bank holds an uncorrected error or says the
/// processor's context is corrupt (the kernel has no way to contain one yet, and would be
/// guessing if it went on). Then the valid banks and MCIP are cleared and the interrupted
/// context goes on (MCIP left set makes the next machine check a shutdown). Anything else
/// is fatal. Reads no CPU-local data: it may interrupt anything, even `swapgs` in progress.
fn machine_check() -> Result<(), FatalMachineCheck> {
    if cpuid(1)[3] & CPUID_MCA == 0 {
        return Err(FatalMachineCheck {
            status: 0,
            bank: None,
        });
    }
    // SAFETY: CPL 0; MCA says these MSRs exist, and reading them has no side effect.
    let status = unsafe { rdmsr(MCG_STATUS) };
    // SAFETY: as above.
    let banks = (unsafe { rdmsr(MCG_CAP) } & 0xFF).min(MC_BANKS) as u32;
    let mut bad_bank = None;
    let mut valid = [false; MC_BANKS as usize];
    for (bank, valid) in (0..banks).zip(valid.iter_mut()) {
        // SAFETY: CPL 0; `bank` is one of the banks MCG_CAP says exist.
        let bank_status = unsafe { rdmsr(MC0_STATUS + 4 * bank) };
        *valid = bank_status & MC_VAL != 0;
        if *valid && bank_status & (MC_UC | MC_PCC) != 0 && bad_bank.is_none() {
            bad_bank = Some((u64::from(bank), bank_status));
        }
    }
    if bad_bank.is_some() || status & (MCG_RIPV | MCG_MCIP) != MCG_RIPV | MCG_MCIP {
        return Err(FatalMachineCheck {
            status,
            bank: bad_bank,
        });
    }
    for (bank, valid) in (0..banks).zip(valid.iter()) {
        if *valid {
            // SAFETY: CPL 0; a bank MCG_CAP says exists. Zero clears its record.
            unsafe { wrmsr(MC0_STATUS + 4 * bank, 0) };
        }
    }
    // SAFETY: CPL 0; clearing MCIP is what the handler of a machine check must do before it
    // resumes, and only affects how the next machine check is delivered.
    unsafe { wrmsr(MCG_STATUS, status & !MCG_MCIP) };
    Ok(())
}

/// RFLAGS.AC: with SMAP on, the only thing that lets the kernel touch user pages.
const RFLAGS_AC: u64 = 1 << 18;
/// Kernel entries that found AC set after `exception_saved` should have cleared it.
static AC_LEAKS: AtomicU64 = AtomicU64::new(0);

/// How many entries from ring 3 reached kernel code with RFLAGS.AC set, for the boot proof.
pub fn ac_leaks() -> u64 {
    AC_LEAKS.load(Ordering::Relaxed)
}

/// RFLAGS.
fn read_rflags() -> u64 {
    let value: u64;
    // SAFETY: PUSHFQ and POP touch only the stack and the output register.
    unsafe { asm!("pushfq", "pop {}", out(reg) value, options(nomem, preserves_flags)) };
    value
}

/// Whether this processor takes interrupts now (RFLAGS.IF).
pub fn interrupts_enabled() -> bool {
    read_rflags() & (1 << 9) != 0
}

/// CR2: the linear address of the last page fault.
fn read_cr2() -> u64 {
    let value: u64;
    // SAFETY: as `read_cr4`.
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack)) };
    value
}

/// CPUID `leaf`, sub-leaf 0: `[eax, ebx, ecx, edx]`.
pub fn cpuid(leaf: u32) -> [u32; 4] {
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32);
    // SAFETY: CPUID is available on every x86-64 processor and reads no memory. RBX is
    // reserved by LLVM, so it is moved out through another register and restored.
    unsafe {
        asm!(
            "mov {save:r}, rbx",
            "cpuid",
            "xchg {save:r}, rbx",
            save = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") 0u32 => ecx,
            out("edx") edx,
            options(nomem, nostack, preserves_flags),
        );
    }
    [eax, ebx, ecx, edx]
}

/// CPUID `leaf` with sub-leaf `sub` in ECX: `[eax, ebx, ecx, edx]`.
pub fn cpuid_subleaf(leaf: u32, sub: u32) -> [u32; 4] {
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32);
    // SAFETY: CPUID is available on every x86-64 processor and reads no memory. RBX is
    // reserved by LLVM, so it is moved out through another register and restored.
    unsafe {
        asm!(
            "mov {save:r}, rbx",
            "cpuid",
            "xchg {save:r}, rbx",
            save = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") sub => ecx,
            out("edx") edx,
            options(nomem, nostack, preserves_flags),
        );
    }
    [eax, ebx, ecx, edx]
}

/// Whether the processor supports the execute-disable bit (CPUID 8000_0001h EDX bit 20).
pub fn nx_supported() -> bool {
    cpuid(0x8000_0000)[0] >= 0x8000_0001 && cpuid(0x8000_0001)[3] & (1 << 20) != 0
}

/// Whether the processor supports x2APIC mode (CPUID 1 ECX bit 21).
pub fn x2apic_supported() -> bool {
    cpuid(1)[2] & (1 << 21) != 0
}

/// Reads model-specific register `msr`.
///
/// # Safety
///
/// CPL 0, and `msr` must exist on this processor (else #GP) and have no read side effects
/// this code does not expect.
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: the caller's contract; RDMSR reads no memory.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Writes `value` to model-specific register `msr`.
///
/// # Safety
///
/// CPL 0, `msr` must exist and `value` must be valid for it, and the write's effect on the
/// processor must be one the caller intends and keeps memory-safe.
pub unsafe fn wrmsr(msr: u32, value: u64) {
    // SAFETY: the caller's contract; WRMSR reads no memory.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// `value` if it is below `limit`, and 0 if it is not, chosen by a conditional move that
/// depends on the comparison and on no prediction: the Mask form of [`crate::nospec`]
/// (ADR-0017 Z9). It is written in assembly because a compiler may turn the same thing
/// written in Rust back into a branch.
#[inline]
#[must_use]
pub fn below_or_zero(value: u64, limit: u64) -> u64 {
    let mut kept = value;
    // SAFETY: a comparison and a conditional move between registers. Nothing reads or writes
    // memory or the stack. The flags change, so the block does not say it preserves them.
    unsafe {
        asm!(
            "cmp {kept}, {limit}",
            "cmovae {kept}, {zero}",
            kept = inout(reg) kept,
            limit = in(reg) limit,
            zero = in(reg) 0u64,
            options(pure, nomem, nostack),
        );
    }
    kept
}

/// `value`, once every instruction before this one has completed: LFENCE, the Fence form of
/// [`crate::nospec`] (ADR-0017 Z9). The value passes through the block, so nothing that
/// uses the result can be placed before the LFENCE.
#[inline]
#[must_use]
pub fn after_fence(value: u64) -> u64 {
    let mut kept = value;
    // SAFETY: LFENCE reads and writes no register, no memory and no flag. RAX is named only
    // to carry the value through the block.
    unsafe { asm!("lfence", inout("rax") kept, options(nomem, nostack)) };
    kept
}

/// `IA32_SPEC_CTRL`: the speculation controls a processor keeps set.
const SPEC_CTRL: u32 = 0x48;
/// `IA32_PRED_CMD`: a write of bit 0 is one IBPB.
const PRED_CMD: u32 = 0x49;
/// `IA32_ARCH_CAPABILITIES`.
const ARCH_CAPABILITIES: u32 = 0x10A;
/// AMD's MSR C001_1029, whose bit 1 makes LFENCE dispatch-serializing.
const DE_CFG: u32 = 0xC001_1029;
const DE_CFG_LFENCE: u64 = 1 << 1;
/// EFER.AIBRSE: AMD's automatic IBRS.
const EFER_AIBRSE: u64 = 1 << 21;

/// What this processor reports about speculation (ADR-0017 Z1): the CPUID leaves
/// `speculation` decodes, each read only if the processor has it, and
/// `IA32_ARCH_CAPABILITIES` where CPUID says it exists.
#[must_use]
pub fn read_speculation() -> crate::speculation::Reported {
    let leaf0 = cpuid(0);
    let leaf1 = cpuid(1);
    let highest_extended = cpuid(0x8000_0000)[0];
    let mut leaf7 = [[0; 4]; 3];
    if leaf0[0] >= 7 {
        leaf7[0] = cpuid_subleaf(7, 0);
        // Subleaf 0's EAX is the last subleaf the processor has.
        for subleaf in 1..=2 {
            if leaf7[0][0] >= subleaf {
                leaf7[subleaf as usize] = cpuid_subleaf(7, subleaf);
            }
        }
    }
    let extended = |leaf: u32| {
        if highest_extended >= leaf {
            cpuid(leaf)
        } else {
            [0; 4]
        }
    };
    let arch_capabilities = if leaf7[0][3] & (1 << 29) != 0 {
        // SAFETY: CPL 0; CPUID.07H.00H:EDX[29] says this processor has the register, and
        // reading it has no side effect.
        unsafe { rdmsr(ARCH_CAPABILITIES) }
    } else {
        0
    };
    crate::speculation::Reported {
        leaf0,
        leaf1: [leaf1[0], leaf1[2]],
        leaf7,
        ext8_ebx: extended(0x8000_0008)[1],
        ext21: extended(0x8000_0021),
        arch_capabilities,
    }
}

/// Sets this processor's speculation controls and records what it found (ADR-0017 Z1, Z2,
/// Z5, Z9). Every processor runs this on itself as it starts, from [`load_tables`], before it
/// runs anything else. A processor's reset clears these registers, so a resume from sleep
/// will have to come back through here.
///
/// Each register is written only where `Plan::decide` found the bit that says the
/// processor has it, and is read back: a processor whose registers do not hold what was
/// written says so in its record.
fn init_speculation(me: ThisCpu<'_>) {
    use crate::speculation::{self, Lfence, Plan};
    let plan = Plan::decide(&read_speculation());
    let mut held = true;
    if let Some(value) = plan.spec_ctrl {
        // SAFETY: CPL 0; the plan names a value only where CPUID enumerates the register,
        // and sets only bits CPUID enumerates. The bits restrict the processor's branch
        // predictors and change nothing about memory.
        unsafe { wrmsr(SPEC_CTRL, value) };
        // SAFETY: as above; reading it has no side effect.
        held &= unsafe { rdmsr(SPEC_CTRL) } == value;
    }
    if plan.automatic_ibrs {
        // SAFETY: CPL 0; EFER exists on every x86-64 processor, and CPUID Fn8000_0021_EAX[8]
        // says this one has AIBRSE, which only restricts the predictors in ring 0.
        let efer = unsafe { rdmsr(EFER) };
        // SAFETY: as above; every other bit is written back as it was read.
        unsafe { wrmsr(EFER, efer | EFER_AIBRSE) };
        // SAFETY: as above.
        held &= unsafe { rdmsr(EFER) } & EFER_AIBRSE != 0;
    }
    if plan.lfence == Lfence::KernelSets {
        // SAFETY: CPL 0; the plan asks for this only on an AMD family that AMD's mitigation
        // G-2 says has the register, and never under a hypervisor. Bit 1 makes LFENCE wait
        // for the instructions before it; every other bit is written back as it was read.
        let value = unsafe { rdmsr(DE_CFG) };
        // SAFETY: as above.
        unsafe { wrmsr(DE_CFG, value | DE_CFG_LFENCE) };
        // SAFETY: as above.
        held &= unsafe { rdmsr(DE_CFG) } & DE_CFG_LFENCE != 0;
    }
    let cpu = me.index().get();
    if cpu == CpuIndex::BOOT.get() {
        // The maker and family are the machine's: the boot processor chooses for all.
        crate::nospec::set_form(plan.form);
    }
    speculation::record(cpu, plan, held);
}

/// Tells this processor that the thread it is about to resume runs `process` in ring 3, and
/// issues an IBPB if that is not the process it ran there last (ADR-0017 Z3). Only at the
/// scheduler's switch point, which has interrupts off, so nothing else on this processor
/// runs between the note and the barrier.
pub fn entering_process(process: u64, switch: &Switch) {
    if crate::speculation::enters(this_cpu(switch.here()).index().get(), process) {
        // SAFETY: CPL 0; `enters` says yes only where this processor recorded that it has
        // the IBPB command, so the register exists. Bit 0 is the command, and it changes
        // nothing but what the branch predictors hold.
        unsafe { wrmsr(PRED_CMD, 1) };
    }
}

/// Issues one IBPB on this processor if it has the command, and says whether it did. For
/// the boot's measurement of what one costs; the scheduler's own are [`entering_process`]'s.
/// `me` is the index the boot flow read (ADR-0020 S3; TASKS item 93).
pub fn issue_ibpb(me: ThisCpu<'_>) -> bool {
    let offered = crate::speculation::recorded(me.index().get()).is_some_and(|(plan, _)| plan.ibpb);
    if offered {
        // SAFETY: as in `entering_process`.
        unsafe { wrmsr(PRED_CMD, 1) };
    }
    offered
}

/// Runs `f` with interrupts disabled on this processor, restoring the previous state.
///
/// Every lock that a thread can hold while preemptible is taken inside this: otherwise a
/// thread preempted while holding it makes every other thread on the same processor that
/// wants it spin away its whole time slice. The `cli` and `sti` are compiler barriers (no
/// `nomem`): a lock's atomics must not move outside the disabled region.
pub fn without_interrupts<T>(f: impl FnOnce() -> T) -> T {
    let flags: u64;
    // SAFETY: reads RFLAGS and disables interrupts; CPL 0. Restored below.
    unsafe { asm!("pushfq", "pop {}", "cli", out(reg) flags, options(preserves_flags)) };
    let result = f();
    if flags & (1 << 9) != 0 {
        // SAFETY: interrupts were enabled on entry, so re-enabling restores that state.
        unsafe { asm!("sti", options(nostack)) };
    }
    result
}

/// [`without_interrupts`], with the section's witness (ADR-0020 S3; TASKS item 93): `trap::section`,
/// which makes the witness, as its documentation says. `f` must not enable interrupts, open an
/// interrupt window, idle, yield, sleep, block or exit.
pub fn without_interrupts_here<T>(f: impl for<'a> FnOnce(crate::trap::Here<'a>) -> T) -> T {
    crate::trap::section(f)
}

/// This processor's CPU index, read under a witness that the reader cannot leave the processor
/// (ADR-0020 S3; TASKS item 93), and branded with the witness's life ([`ThisCpu`]). It is the only
/// way to read the index: a call without a witness does not build.
pub fn this_cpu(here: crate::trap::Here<'_>) -> ThisCpu<'_> {
    ThisCpu {
        index: read_index(),
        here,
    }
}

/// This processor's CPU index, read once under a witness and branded with its life (ADR-0020 S3;
/// TASKS item 93, the coordinating session's ruling Q4).
///
/// Every lookup of this processor's own entry of a per-processor table goes through it
/// (`PerCpu::this`), and is lent for that life. So no such entry is used after its reader might
/// have left the processor, and one read of the index serves every lookup under one witness, as
/// one read served them before.
///
/// [`ThisCpu::index`] gives the plain index, as a value only: one to compare, print or record as
/// a fact about the moment it was read.
#[derive(Clone, Copy, Debug)]
pub struct ThisCpu<'a> {
    index: CpuIndex,
    here: crate::trap::Here<'a>,
}

impl<'a> ThisCpu<'a> {
    /// The index, as a value (see the type's documentation).
    #[inline]
    pub fn index(self) -> CpuIndex {
        self.index
    }

    /// The witness it was read under.
    #[inline]
    pub fn here(self) -> crate::trap::Here<'a> {
        self.here
    }
}

/// The boot or idle flow's witness (ADR-0020 S2, point 4; TASKS item 93, the coordinating
/// session's rulings Q6 and on `cba7462b`): `trap::flow`, which makes the check and the witness,
/// as its documentation says. `Some` if the context running on this processor is the processor's
/// boot or idle flow, `None` on a thread. Each flow takes it once, at its entry, and passes it
/// down.
pub fn flow() -> Option<crate::trap::Pinned<'static>> {
    crate::trap::flow()
}

/// CR3: the physical address of the live PML4 (and, with PCIDs off, nothing else).
pub fn read_cr3() -> u64 {
    let value: u64;
    // SAFETY: as `read_cr4`.
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack)) };
    value
}

/// Loads `root` into CR3, which also flushes every non-global TLB entry.
///
/// # Safety
///
/// `root` must be a complete PML4 that maps everything the running code touches next (its
/// code, stack and data): in practice the kernel's, or a process PML4 sharing its half.
pub unsafe fn write_cr3(root: u64) {
    // The record and the write together, with interrupts off (review finding 1).
    crate::tlb::switch_space(root, || {
        // SAFETY: the caller's contract.
        unsafe { load_cr3(root) };
    });
}

/// The CR3 write alone, which also flushes every non-global TLB entry: the shootdown protocol's
/// record of what this processor runs on is the caller's to keep (`tlb`).
///
/// # Safety
///
/// As for [`write_cr3`].
pub(crate) unsafe fn load_cr3(root: u64) {
    // SAFETY: the caller's contract. The flags are not preserved: Intel leaves them undefined
    // after a move to a control register.
    unsafe { asm!("mov cr3, {}", in(reg) root, options(nostack)) };
}

/// The furthest a backtrace reads above the starting frame pointer, and the largest single
/// frame it will step over.
const BACKTRACE_SPAN: u64 = 256 * 1024;
const BACKTRACE_MAX_FRAME: u64 = 64 * 1024;

/// Captures the current call stack's return addresses into `out` (innermost first) by
/// walking the frame-pointer chain, and returns how many it found. It reads only words in
/// a bounded window above the starting frame, so a corrupt chain cannot make it fault.
#[inline(always)]
pub fn capture_backtrace(out: &mut [u64]) -> usize {
    let rbp: u64;
    // SAFETY: reads RBP; no memory, no stack, flags preserved.
    unsafe { asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack, preserves_flags)) };
    let base = rbp;
    // On a stack in the stack region the window ends at the top of the stack's slot: the next
    // slot's unmapped space lies past it (ADR-0004 Amendment 1, A2).
    let last = match crate::stack::slot_top(base) {
        Some(top) => top
            .saturating_sub(8)
            .min(base.saturating_add(BACKTRACE_SPAN)),
        None => base.saturating_add(BACKTRACE_SPAN),
    };
    crate::unwind::walk(
        rbp,
        BACKTRACE_MAX_FRAME,
        |addr| {
            if addr < base || addr > last || addr & 7 != 0 {
                return None;
            }
            let p = ptr::with_exposed_provenance::<u64>(addr as usize);
            // SAFETY: `addr` is 8-aligned and within `BACKTRACE_SPAN` bytes above the starting
            // frame pointer, and, on a stack in the stack region, below the top of its slot, so
            // it lies in the running stack, which is mapped.
            Some(unsafe { p.read_volatile() })
        },
        out,
    )
}

/// The TSC, read only once every earlier instruction has executed, so a reading cannot be
/// taken before the lock that orders it is held: RDTSCP (Intel SDM Vol. 2B, RDTSCP; AMD
/// APM Vol. 3) if `use_rdtscp` and this processor has it, else LFENCE then RDTSC.
pub fn ordered_tsc(use_rdtscp: bool) -> u64 {
    let fence = if use_rdtscp {
        TscFence::detect()
    } else {
        TscFence::LFENCE
    };
    fence.after()
}

/// How two TSC readings are ordered around what runs between them, so that they enclose it
/// (TASKS item 78, `docs/research/2026-11-21-apic-timer-period.md` §2). Neither RDTSC nor
/// RDMSR is serializing on either maker's processors, an x2APIC register's RDMSR included
/// (Intel SDM Vol. 3A §13.12.3; AMD APM Vol. 2 §16.11.2): unfenced, a reading may be taken
/// after the instruction it should precede, or before the one it should follow.
///
/// The form is private, so the RDTSCP form comes only from [`TscFence::detect`], which found
/// RDTSCP on this processor: safe code cannot ask for an instruction the processor lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TscFence(Fence);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fence {
    /// RDTSCP, the first reading followed by LFENCE. Intel: LFENCE is always a barrier, and
    /// RDTSCP waits for every earlier instruction. AMD: RDTSCP retires every older instruction
    /// before it reads and is dispatch serializing, whatever MSR C001_1029 holds.
    Rdtscp,
    /// No RDTSCP: LFENCE beside RDTSC. Ordered where LFENCE is a barrier: on Intel always; on
    /// AMD where CPUID Fn8000_0021_EAX[2] says so, or where the kernel set MSR C001_1029 bit 1
    /// (ADR-0017, `Lfence::KernelSets`). Under a hypervisor on AMD without either, unordered.
    Lfence,
}

impl TscFence {
    /// LFENCE beside RDTSC, which every x86-64 processor has.
    pub const LFENCE: Self = Self(Fence::Lfence);

    /// RDTSCP where CPUID Fn8000_0001_EDX[27] reports it, else LFENCE. Read once, not for each
    /// reading: CPUID leaves the guest under a hypervisor.
    pub fn detect() -> Self {
        if cpuid(0x8000_0000)[0] >= 0x8000_0001 && cpuid(0x8000_0001)[3] & (1 << 27) != 0 {
            Self(Fence::Rdtscp)
        } else {
            Self::LFENCE
        }
    }

    /// The TSC, read before any later instruction starts: RDTSCP or RDTSC, then LFENCE.
    pub fn before(self) -> u64 {
        let (low, high): (u32, u32);
        match self.0 {
            Fence::Rdtscp => {
                // SAFETY: `Fence::Rdtscp` is private and made only by `detect`, which found
                // RDTSCP on this processor; RDTSCP writes EDX:EAX and ECX (IA32_TSC_AUX, unused here) and
                // reads no memory, and LFENCE reads and writes nothing. No `preserves_flags`:
                // Intel's entry for LFENCE does not say what it does to the flags.
                unsafe {
                    asm!("rdtscp", "lfence", out("eax") low, out("edx") high, out("ecx") _, options(nomem, nostack));
                }
            }
            Fence::Lfence => {
                // SAFETY: RDTSC and LFENCE exist on every x86-64 processor and read no memory.
                // No `preserves_flags`, as above.
                unsafe {
                    asm!("rdtsc", "lfence", out("eax") low, out("edx") high, options(nomem, nostack));
                }
            }
        }
        (u64::from(high) << 32) | u64::from(low)
    }

    /// The TSC, read once every earlier instruction has executed: RDTSCP, or LFENCE then
    /// RDTSC.
    pub fn after(self) -> u64 {
        let (low, high): (u32, u32);
        match self.0 {
            Fence::Rdtscp => {
                // SAFETY: as in `before`; RDTSCP alone changes no flag.
                unsafe {
                    asm!("rdtscp", out("eax") low, out("edx") high, out("ecx") _, options(nomem, nostack, preserves_flags));
                }
            }
            Fence::Lfence => {
                // SAFETY: LFENCE and RDTSC exist on every x86-64 processor and read no memory.
                // No `preserves_flags`: Intel's entry for LFENCE does not say what it does to
                // the flags.
                unsafe {
                    asm!("lfence", "rdtsc", out("eax") low, out("edx") high, options(nomem, nostack));
                }
            }
        }
        (u64::from(high) << 32) | u64::from(low)
    }
}

/// The time-stamp counter.
pub fn rdtsc() -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: RDTSC is available on every x86-64 processor (CR4.TSD is clear: the kernel
    // runs at CPL 0 anyway) and reads no memory.
    unsafe {
        asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags))
    };
    (u64::from(high) << 32) | u64::from(low)
}

/// Opens a one-instruction window in which a pending interrupt is taken, then disables
/// interrupts again.
///
/// Callers poll with this rather than halting: a halt waits for an interrupt that may never
/// come (a missing EOI blocks every later one at the same priority), so the caller could
/// never check its own deadline.
pub fn interrupt_window() {
    if tables_ready() {
        // SAFETY: CPL 0. Interrupts are recognized after the instruction that follows STI, so
        // a pending one is taken after the NOP and before CLI. Every vector that can arrive
        // has a gate (`tables_ready`: the timer, the spurious vector and the exceptions).
        unsafe { asm!("sti", "nop", "cli", options(nostack)) };
    }
}

/// IA32_EFER.
const EFER: u32 = 0xC000_0080;
/// IA32_EFER.NXE: the NX bit in page-table entries is honoured (and is no longer reserved).
const EFER_NXE: u64 = 1 << 11;
/// CR0.WP: supervisor writes honour read-only pages.
const CR0_WP: u64 = 1 << 16;

/// Enables NX and supervisor write protection, then loads `root` into CR3.
///
/// # Safety
///
/// Call on the boot processor at CPL 0 with interrupts disabled, after checking
/// [`nx_supported`]. `root` must be the PML4 of complete tables that map, with the right
/// permissions, everything the kernel touches from here on: its code, its stack, its data,
/// the boot info and the memory it allocates from, and the tables themselves.
pub unsafe fn load_page_tables(root: u64) {
    // SAFETY: the caller's contract. EFER is read and written back with only NXE added
    // (NX is supported, so the bit is defined). WP only restricts supervisor writes to
    // pages already marked read-only. Loading CR3 switches to tables the caller vouches
    // for and flushes every non-global TLB entry. The flags are not preserved: `or` writes them.
    unsafe {
        asm!(
            "mov ecx, {efer}",
            "rdmsr",
            "or eax, {nxe}",
            "wrmsr",
            "mov {tmp}, cr0",
            "or {tmp}, {wp}",
            "mov cr0, {tmp}",
            "mov cr3, {root}",
            efer = const EFER,
            nxe = const EFER_NXE,
            wp = const CR0_WP,
            root = in(reg) root,
            tmp = out(reg) _,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            options(nostack),
        );
    }
}

/// What the probe functions return when the access completed (no #PF error code is this).
const COMPLETED: u64 = u64::MAX;

/// `mov rax, -1; ret`: executed only if NX fails to stop the fetch, in which case the jump
/// probe returns [`COMPLETED`]. It is writable data, so it must never be executable.
static mut PROBE_CODE: [u8; 8] = PROBE_RETURN;

/// What a probe access did. Its `Debug` tells a faulting address as an offset in its window,
/// since a guard probe's is in the kernel stacks' region (ADR-0004 Amendment 2, A6.2; TASKS item
/// 70, Q8).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// The access completed: the page allowed it.
    Completed,
    /// It raised #PF with this error code at this linear address (CR2).
    Faulted {
        /// The #PF error code (SDM Vol. 3A §4.7).
        error: u64,
        /// CR2.
        address: u64,
    },
    /// The address is in a window of device memory: not read.
    Refused,
}

impl core::fmt::Debug for Probe {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Completed => f.write_str("Completed"),
            Self::Faulted { error, address } => f
                .debug_struct("Faulted")
                .field("error", &error)
                .field("address", &format_args!("{}", crate::phys::told(address)))
                .finish(),
            Self::Refused => f.write_str("Refused"),
        }
    }
}

/// Arms the probe for a fault at `fault_rip`, runs `access`, and disarms it.
///
/// The three run in one interrupts-off section, and the processor's CPU-local area is read under
/// its witness (ADR-0020 S3; TASKS item 93): the fault `access` may take is taken on the processor
/// that armed the probe, whose trap entry reads the same area, and the record read back is that
/// processor's. Before TASKS item 93 interrupts stayed as the caller had them; now they are off
/// for the one access and its fault.
fn run_probe(fault_rip: u64, access: impl FnOnce() -> u64) -> Probe {
    // Per CPU: probes and user copies on different processors never disarm each other.
    without_interrupts_here(|at| {
        let here = local(at);
        here.probe_rip.store(fault_rip, Ordering::SeqCst);
        let result = access();
        here.probe_rip.store(0, Ordering::SeqCst);
        if result == COMPLETED {
            Probe::Completed
        } else {
            Probe::Faulted {
                error: result,
                address: here.probe_address.load(Ordering::SeqCst),
            }
        }
    })
}

/// Reads the byte at `addr` through a fault-tolerant instruction and reports what happened.
/// One byte of RAM read this way harms nothing (the value only goes into the report);
/// device memory is refused, because reading it could have side effects.
pub fn probe_read(addr: u64) -> Probe {
    if crate::vm::in_device_window(addr) {
        return Probe::Refused;
    }
    // SAFETY: `read_byte` faults at its first instruction or reads one byte and returns;
    // `addr` is not in the device windows, so a successful read has no side effect.
    run_probe((read_byte as *const ()).addr() as u64, || unsafe {
        read_byte(addr)
    })
}

/// Writes `value` to the byte at `addr`.
///
/// # Safety
///
/// `value` must be the byte already at `addr` and nothing may change it concurrently, so a
/// write that completes changes nothing; `addr` must not be MMIO.
pub unsafe fn probe_write(addr: u64, value: u8) -> Probe {
    // SAFETY: the caller's contract; `write_byte` faults at its first instruction or
    // stores `value` (unchanged memory) and returns.
    run_probe((write_byte as *const ()).addr() as u64, || unsafe {
        write_byte(addr, value)
    })
}

/// Jumps into [`PROBE_CODE`], writable data, which NX must refuse to execute. Returns the
/// probed address too.
pub fn probe_execute_data() -> (u64, Probe) {
    let target = (&raw const PROBE_CODE).addr() as u64;
    // SAFETY: `PROBE_CODE` holds `mov rax, -1; ret`, so the jump either faults or runs it.
    let probe = unsafe { probe_execute(target) };
    (target, probe)
}

/// Whether SMAP is enabled (so user accesses need `stac`/`clac`).
static SMAP_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Whether UMIP is on (ring 3 cannot read the descriptor-table registers).
static UMIP_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Whether UMIP is on, for the boot proof.
pub fn umip_on() -> bool {
    UMIP_ON.load(Ordering::Relaxed)
}

/// Page faults the user-copy instruction took and survived.
static USER_COPY_FAULTS: AtomicU64 = AtomicU64::new(0);

/// How many user copies a page fault cut short.
pub fn user_copy_faults() -> u64 {
    USER_COPY_FAULTS.load(Ordering::Relaxed)
}

/// Copies `len` bytes from `src` to `dst`, one of which is a user address, and reports
/// whether it finished: `false` means a page fault stopped it. Runs with interrupts on, so
/// a long copy stays preemptible; `dispatch` recognizes a fault at the copy instruction by
/// its address, not by per-CPU state a preempted copy would disturb.
///
/// # Safety
///
/// One of `dst` and `src` must be a live kernel buffer of `len` bytes, which this call may
/// read or write. The other must be a range the caller checked against the calling
/// process's own tables, on this processor's live address space: user memory, so that a
/// fault in it is recovered (a fault on the kernel side is a kernel bug, and stops here).
pub unsafe fn copy_user(dst: u64, src: u64, len: u64) -> bool {
    let smap = SMAP_ON.load(Ordering::Relaxed);
    if smap {
        // SAFETY: STAC only sets RFLAGS.AC, allowing supervisor access to user pages until
        // the CLAC below. An interrupt in between saves AC in its frame, and every handler
        // runs with it clear (`exception_saved`), until IRETQ restores it for this copy.
        unsafe { asm!("stac", options(nomem, nostack)) };
    }
    // SAFETY: the caller's contract; a fault in the user range resumes at `probe_recover`
    // instead of taking the kernel down.
    let result = unsafe { user_copy(dst, src, 0, len) };
    if smap {
        // SAFETY: CLAC only clears RFLAGS.AC again.
        unsafe { asm!("clac", options(nomem, nostack)) };
    }
    result == COMPLETED
}

/// The boot proof of user-copy fault recovery: copies 16 bytes from a user address that no
/// address space maps into `buffer`, and returns whether the copy completed (it must not).
pub fn probe_user_copy_fault(buffer: &mut [u8; 16]) -> bool {
    const NOWHERE: u64 = 0x0000_2000_0000_0000;
    let destination = buffer.as_mut_ptr().expose_provenance() as u64;
    // SAFETY: `buffer` is a live 16-byte kernel buffer; `NOWHERE` is a user address not mapped
    // in the kernel's tables, the ones in force here, so the first access faults and the
    // fault is recovered at the copy instruction.
    unsafe { copy_user(destination, NOWHERE, 16) }
}

/// Turns interrupts on for a system call, which runs on its own thread's stack.
pub fn enable_interrupts() {
    if tables_ready() {
        // SAFETY: CPL 0; every vector that can arrive has a gate (`tables_ready`). A lock held
        // across this is a deadlock of the caller's, not a memory-safety matter.
        unsafe { asm!("sti", options(nostack)) };
    }
}

/// Turns interrupts off on the way back out of a system call.
pub fn disable_interrupts() {
    // SAFETY: CPL 0; CLI only clears RFLAGS.IF.
    unsafe { asm!("cli", options(nostack)) };
}

/// `rep movsb` from RSI to RDI, RCX bytes (the fourth argument), as the first instruction,
/// so a fault has RIP = `user_copy` and resumes at `probe_recover`.
#[unsafe(naked)]
unsafe extern "sysv64" fn user_copy(_dst: u64, _src: u64, _unused: u64, _len: u64) -> u64 {
    naked_asm!("rep movsb", "mov rax, -1", "ret", "int3");
}

/// The bytes of `mov rax, -1; ret`, for a probe target that must not be executable.
pub const PROBE_RETURN: [u8; 8] = [0x48, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF, 0xC3];

/// Jumps to `target` and reports whether the fetch faulted.
///
/// # Safety
///
/// If the fetch is allowed, the code at `target` runs: it must be [`PROBE_RETURN`].
pub unsafe fn probe_execute(target: u64) -> Probe {
    // SAFETY: the fetch at `target` either faults (and resumes at `probe_recover`, which
    // returns to this caller) or runs `PROBE_RETURN` (the caller's contract), which only
    // sets RAX and returns.
    run_probe(target, || unsafe { jump(target) })
}

/// IA32_PAT.
const IA32_PAT: u32 = 0x277;
/// Each CPU index whose processor's PAT holds the two memory types the page tables use.
pub(crate) static PAT_READY: PerCpu<core::sync::atomic::AtomicBool> = PerCpu::new(
    "origin_frame::cpu::PAT_READY",
    core::sync::atomic::AtomicBool::new(false),
    || core::sync::atomic::AtomicBool::new(false),
);

/// Checks that this processor's PAT has entry 0 write-back (06h) and entry 3 uncached
/// (00h), the memory types `paging::Cache` selects (Intel SDM Vol. 3A §11.12.2,
/// Table 11-10), and marks this processor's CPU index if so. These are the architectural
/// reset values; a firmware that changed them would make uncached mappings cacheable, so the
/// kernel then refuses to map device registers rather than reprogramming caches it cannot
/// see. Returns the PAT. `me` is the index its flow read (ADR-0020 S3; TASKS item 93).
pub fn check_pat(me: ThisCpu<'_>) -> u64 {
    // SAFETY: CPL 0; IA32_PAT exists on every x86-64 processor (CPUID 1 EDX bit 16) and
    // reading it has no side effect.
    let pat = unsafe { rdmsr(IA32_PAT) };
    if pat & 0xFF == 0x06
        && (pat >> 24) & 0xFF == 0x00
        && let Some(ready) = PAT_READY.this(me)
    {
        ready.store(true, Ordering::Relaxed);
    }
    pat
}

/// How many of `cpus` passed [`check_pat`]. Only the processors asked about count: an AP
/// that checked its PAT but never came online (it was parked, `smp::park`) is not counted.
/// An AP marks its index before its start-up handshake, which orders the mark before the
/// boot processor sees it online.
pub fn pat_ready(cpus: &[CpuIndex]) -> usize {
    cpus.iter()
        .filter(|c| {
            PAT_READY
                .get(c.get())
                .is_some_and(|r| r.load(Ordering::Relaxed))
        })
        .count()
}

/// CR4.PGE: global pages (Intel SDM Vol. 3A §4.10.2.4).
const CR4_PGE: u64 = 1 << 7;

/// Turns on global pages (CR4.PGE) where supported (CPUID 1 EDX bit 13): kernel-half
/// translations then outlive CR3 loads. Clearing and setting the bit also invalidates every
/// TLB entry, global ones included, so nothing cached under earlier tables (the firmware's
/// or the loader's) survives. Returns whether global pages are on.
///
/// # Safety
///
/// CPL 0, and the live tables mark global only mappings that every address space shares
/// (the kernel half); a global user mapping would leak between processes.
pub unsafe fn enable_global_pages() -> bool {
    if cpuid(1)[3] & (1 << 13) == 0 {
        return false;
    }
    // SAFETY: the caller's contract. Each write changes only CR4.PGE, which affects only
    // which translations the TLB keeps. The flags are not preserved: `and` and `or` write them.
    unsafe {
        asm!(
            "mov {tmp}, cr4",
            "and {tmp}, {clear}",
            "mov cr4, {tmp}",
            "or {tmp}, {pge}",
            "mov cr4, {tmp}",
            clear = in(reg) !CR4_PGE,
            pge = in(reg) CR4_PGE,
            tmp = out(reg) _,
            options(nostack),
        );
    }
    true
}

/// Invalidates every TLB entry on this processor, global ones included: CR4.PGE toggled
/// when it is set (a CR3 load keeps global entries), else a CR3 reload.
pub fn flush_tlb_all() {
    without_interrupts(|| {
        // SAFETY: CPL 0. CR4 is written back with the value it had, after a write that
        // differs only in PGE; CR3 is reloaded with its own value. Neither changes which
        // memory is mapped, only what the TLB holds. Interrupts are off, so no handler sees
        // CR4 between the writes.
        unsafe {
            asm!(
                "mov {cr4}, cr4",
                "test {cr4}, {pge}",
                "jz 2f",
                "mov {tmp}, {cr4}",
                "xor {tmp}, {pge}",
                "mov cr4, {tmp}",
                "mov cr4, {cr4}",
                "jmp 3f",
                "2:",
                "mov {tmp}, cr3",
                "mov cr3, {tmp}",
                "3:",
                pge = in(reg) CR4_PGE,
                cr4 = out(reg) _,
                tmp = out(reg) _,
                options(nostack),
            );
        }
    });
}

/// Invalidates this processor's TLB entries that are not global: a CR3 reload with its own value.
pub fn flush_tlb_non_global() {
    without_interrupts(|| {
        // SAFETY: CPL 0. CR3 is reloaded with the value it holds, so nothing about what is
        // mapped changes, only what the TLB holds. Interrupts are off, so no handler sees the
        // processor between the two instructions. The flags are not preserved: Intel leaves
        // them undefined after a move to or from a control register.
        unsafe {
            asm!(
                "mov {tmp}, cr3",
                "mov cr3, {tmp}",
                tmp = out(reg) _,
                options(nostack),
            );
        }
    });
}

/// Invalidates this processor's TLB entries for the page holding `addr`, whatever its size
/// and whether or not it is global (Intel SDM Vol. 2A, INVLPG).
pub fn invlpg(addr: u64) {
    // SAFETY: CPL 0. INVLPG only drops cached translations; it never faults, even for an
    // unmapped or non-canonical address (a non-canonical one is a no-op).
    unsafe { asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags)) };
}

/// CR4.SMEP: supervisor instruction fetches from user pages fault.
const CR4_SMEP: u64 = 1 << 20;
/// CR4.SMAP: supervisor data accesses to user pages fault (unless RFLAGS.AC is set).
const CR4_SMAP: u64 = 1 << 21;
/// CR4.UMIP: `sidt`, `sgdt`, `sldt`, `str` and `smsw` fault at CPL 3, so a program cannot
/// read the kernel's descriptor table addresses, which give away the KASLR slide.
const CR4_UMIP: u64 = 1 << 11;

/// Which of SMEP and SMAP this processor supports (CPUID 7.0 EBX bits 7 and 20).
pub fn smep_smap_supported() -> (bool, bool) {
    if cpuid(0)[0] < 7 {
        return (false, false);
    }
    let ebx = cpuid(7)[1];
    (ebx & (1 << 7) != 0, ebx & (1 << 20) != 0)
}

/// CR0 bits: EM (emulate FPU: must be clear for SSE), MP, TS. NE is set on every processor
/// for its own sake: see [`CR0_NE`].
const CR0_EM: u64 = 1 << 2;
const CR0_MP: u64 = 1 << 1;
const CR0_TS: u64 = 1 << 3;
/// CR4 bits: OSFXSR (FXSAVE and SSE), OSXMMEXCPT (SSE exceptions), OSXSAVE (XSAVE/XGETBV).
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;
const CR4_OSXSAVE: u64 = 1 << 18;

/// Whether SIMD state is managed with XSAVE (else FXSAVE): the boot processor's, recorded once
/// with its format ([`record_boot_fpu`]).
static USE_XSAVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The boot processor's FPU format and the size of every thread's FPU area, recorded once by
/// the boot processor before the first area is allocated (ADR-0020 S6; TASKS item 94).
/// Application processors read it and never write it.
#[derive(Clone, Copy, Debug)]
struct BootFpu {
    format: crate::features::FpuFormat,
    area: usize,
}

static BOOT_FPU: crate::once::SetOnce<BootFpu> = crate::once::SetOnce::new();

/// Gathers the CPUID leaves `features` decodes.
pub fn gather_cpuid() -> crate::features::Cpuid {
    crate::features::Cpuid {
        leaf1: cpuid(1),
        leaf7: cpuid(7),
        leaf_d0: cpuid(0x0D),
        ext1: cpuid(0x8000_0001),
        max_basic: cpuid(0)[0],
        max_ext: cpuid(0x8000_0000)[0],
    }
}

/// Reads an extended control register with XGETBV.
///
/// # Safety
///
/// CPL 0, and CR4.OSXSAVE must be set (else #UD).
unsafe fn xgetbv(reg: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: the caller's contract; XGETBV reads no memory.
    unsafe {
        asm!("xgetbv", in("ecx") reg, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Writes an extended control register with XSETBV.
///
/// # Safety
///
/// CPL 0, CR4.OSXSAVE set, and `value` valid for `reg` (a bad XCR0 is #GP).
unsafe fn xsetbv(reg: u32, value: u64) {
    // SAFETY: the caller's contract; XSETBV reads no memory.
    unsafe {
        asm!("xsetbv", in("ecx") reg, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nomem, nostack, preserves_flags));
    }
}

/// Enables SSE (and AVX where present) on this processor. Returns its FPU format as the
/// processor reports it once XCR0 is set: XCR0 read back, and CPUID.0DH.00H:EBX, the bytes
/// XSAVE then needs (ADR-0020 S6). Without XSAVE, XCR0 0 and the FXSAVE area's 512 bytes. It
/// sets no area size: the boot processor records its own once ([`record_boot_fpu`]).
///
/// The kernel's own code is soft-float (the `x86_64-unknown-none` ABI uses no XMM), so this
/// adds a capability for explicit SIMD and for user programs; it does not change how kernel
/// code is generated.
///
/// # Safety
///
/// CPL 0. Must run once per processor before any code on it uses SSE or XSAVE.
pub unsafe fn enable_simd(id: &crate::features::Cpuid) -> crate::features::FpuFormat {
    use crate::features::{FXSAVE_SIZE, Features, FpuFormat, wanted_xcr0, xsave_usable};
    let features = Features::decode(id);
    // XSAVE only with leaf 0DH to size it (R94-6).
    let xsave = xsave_usable(id);
    // CR0: clear EM and TS, set MP, so SSE instructions run and do not fault to an
    // emulator or a (nonexistent) lazy-FPU handler; set NE, so an x87 error raises #MF
    // (INIT leaves it clear on an AP).
    // SAFETY: the caller's contract; these CR0 bits only configure FPU/SSE behaviour. The flags
    // are not preserved: `and` and `or` write them.
    unsafe {
        asm!(
            "mov {t}, cr0",
            "and {t}, {clear}",
            "or {t}, {set}",
            "mov cr0, {t}",
            clear = in(reg) !(CR0_EM | CR0_TS),
            set = in(reg) CR0_MP | CR0_NE,
            t = out(reg) _,
            options(nomem, nostack),
        );
    }
    // CR4: OSFXSR and OSXMMEXCPT for SSE; OSXSAVE when the CPU has XSAVE.
    let mut cr4_bits = CR4_OSFXSR | CR4_OSXMMEXCPT;
    if xsave {
        cr4_bits |= CR4_OSXSAVE;
    }
    // SAFETY: the caller's contract; these CR4 bits enable FXSAVE/SSE and XSAVE. The flags are
    // not preserved: `or` writes them.
    unsafe {
        asm!(
            "mov {t}, cr4",
            "or {t}, {bits}",
            "mov cr4, {t}",
            bits = in(reg) cr4_bits,
            t = out(reg) _,
            options(nomem, nostack),
        );
    }
    if !xsave {
        return FpuFormat {
            xcr0: 0,
            enabled_size: FXSAVE_SIZE as u32,
        };
    }
    let wanted = wanted_xcr0(&features);
    // SAFETY: CR4.OSXSAVE was just set; `wanted` has x87 and SSE (required) and AVX only
    // when the CPU reports it, so it is a legal XCR0.
    unsafe { xsetbv(0, wanted) };
    FpuFormat {
        // SAFETY: OSXSAVE is set.
        xcr0: unsafe { xgetbv(0) },
        // Read after XSETBV: EBX follows the XCR0 in force on this processor.
        enabled_size: cpuid_subleaf(0x0D, 0)[1],
    }
}

/// Records the boot processor's FPU format, from what [`enable_simd`] returned there, and the
/// size of every thread's FPU area: CPUID.0DH.00H:ECX, the size for every component the
/// processor supports, and never less than what one save in the format writes
/// ([`crate::features::fpu_bytes`]), so the boot processor's state always fits. Returns `false`, and changes nothing, if they were recorded
/// already.
///
/// Called by `Early::enable_simd` alone: once, on the boot processor, before any area is
/// allocated (ADR-0020 S6: the kernel's area size is the boot processor's).
pub(crate) fn record_boot_fpu(
    id: &crate::features::Cpuid,
    format: crate::features::FpuFormat,
) -> bool {
    use crate::features::{FXSAVE_SIZE, xsave_area_size};
    let supported = if format.xcr0 == 0 {
        FXSAVE_SIZE
    } else {
        xsave_area_size(id).unwrap_or(FXSAVE_SIZE)
    };
    // Never less than what one save in its own format writes (R94-10).
    let area = supported.max(crate::features::fpu_bytes(format));
    if BOOT_FPU.set(BootFpu { format, area }).is_err() {
        return false;
    }
    // How every processor saves and restores, from the boot processor alone: an AP that
    // could not use XSAVE is refused below the baseline (`smp::ap_entry`) and never saves
    // (the review of TASKS item 94, R94-5).
    USE_XSAVE.store(format.xcr0 != 0, core::sync::atomic::Ordering::Relaxed);
    true
}

/// The boot processor's FPU format, once it has recorded it.
pub fn boot_fpu_format() -> Option<crate::features::FpuFormat> {
    BOOT_FPU.get().map(|b| b.format)
}

/// The per-thread FPU save area size (bytes) and alignment (64): the boot processor's, or the
/// FXSAVE area's 512 bytes before it recorded one.
pub fn fpu_area_size() -> usize {
    BOOT_FPU
        .get()
        .map_or(crate::features::FXSAVE_SIZE, |b| b.area)
}

/// Prepares a fresh thread's FPU area so a restore from it yields a clean FPU: default
/// control words and no pending state. `area` must be `fpu_area_size()` bytes, 64-byte
/// aligned.
///
/// # Safety
///
/// `area` must point at a writable buffer of at least [`fpu_area_size`] bytes.
pub unsafe fn fpu_init_area(area: u64) {
    let size = fpu_area_size();
    // SAFETY: the caller vouches for the buffer and its size.
    unsafe {
        core::ptr::write_bytes(
            ptr::with_exposed_provenance_mut::<u8>(area as usize),
            0,
            size,
        );
    }
    // Legacy area: FCW = 0x037F (all x87 exceptions masked), MXCSR = 0x1F80 (all SSE
    // exceptions masked). An all-zero MXCSR would unmask every exception.
    let fcw = ptr::with_exposed_provenance_mut::<u16>(area as usize);
    let mxcsr = ptr::with_exposed_provenance_mut::<u32>(area as usize + 24);
    // SAFETY: offset 0 is inside the 512-byte legacy area the buffer begins with.
    unsafe { fcw.write(0x037F) };
    // SAFETY: offset 24 is inside the same legacy area.
    unsafe { mxcsr.write(0x1F80) };
    // The XSAVE header (just after the 512-byte legacy area) stays zero: XSTATE_BV = 0, so
    // XRSTOR puts every component in its init state, which the legacy fields then refine.
}

/// Saves this processor's FPU/SIMD state into `area` (XSAVE if enabled, else FXSAVE).
///
/// # Safety
///
/// CPL 0, SSE/XSAVE enabled (`enable_simd`), and `area` a writable [`fpu_area_size`]-byte,
/// 64-byte-aligned buffer not aliased by another save in progress.
pub unsafe fn fpu_save(area: u64) {
    if USE_XSAVE.load(core::sync::atomic::Ordering::Relaxed) {
        // XSAVE with the full mask (EDX:EAX = all ones): save every enabled component.
        // SAFETY: the caller's contract; the area is large enough (CPUID's maximum). No
        // `preserves_flags` on any of the four blocks that save or restore this state: the
        // option also covers the x87 status word and MXCSR, which is the state they handle.
        unsafe {
            asm!("xsave [{a}]", a = in(reg) area, in("eax") u32::MAX, in("edx") u32::MAX, options(nostack));
        }
    } else {
        // SAFETY: as above; FXSAVE needs 512 bytes, 16-byte aligned.
        unsafe {
            asm!("fxsave [{a}]", a = in(reg) area, options(nostack));
        }
    }
}

/// Restores this processor's FPU/SIMD state from `area`.
///
/// # Safety
///
/// As for [`fpu_save`]; `area` must hold state written by [`fpu_save`] or [`fpu_init_area`].
pub unsafe fn fpu_restore(area: u64) {
    if USE_XSAVE.load(core::sync::atomic::Ordering::Relaxed) {
        // SAFETY: the caller's contract; the area holds a valid XSAVE image. The restore
        // replaces the x87 status word and MXCSR, so it cannot claim to preserve the flags.
        unsafe {
            asm!("xrstor [{a}]", a = in(reg) area, in("eax") u32::MAX, in("edx") u32::MAX, options(nostack));
        }
    } else {
        // SAFETY: as above.
        unsafe {
            asm!("fxrstor [{a}]", a = in(reg) area, options(nostack));
        }
    }
}

/// Enables SMEP and SMAP on this processor, where supported; returns which were enabled.
///
/// # Safety
///
/// CPL 0. The kernel must not rely on executing or accessing user pages from supervisor
/// mode (it has none; a future user-copy path must set RFLAGS.AC around its accesses).
///
/// `me` is the index its flow read (ADR-0020 S3; TASKS item 93).
pub unsafe fn enable_smep_smap(me: ThisCpu<'_>) -> (bool, bool) {
    let (smep, smap) = smep_smap_supported();
    // The boot processor's answer decides what the kernel assumes: an AP that lacks SMAP
    // refuses to start (`smp::ap_entry`) and must not turn it off for everyone else.
    if me.index() == CpuIndex::BOOT {
        SMAP_ON.store(smap, Ordering::Relaxed);
    }
    // UMIP (CPUID.7.0:ECX bit 2): `sidt` from ring 3 would otherwise reveal the kernel's
    // descriptor-table addresses, and with them the KASLR slide (ADR-0004 H3).
    let umip = cpuid(0)[0] >= 7 && cpuid(7)[2] & (1 << 2) != 0;
    if umip && me.index() == CpuIndex::BOOT {
        UMIP_ON.store(true, Ordering::Relaxed);
    }
    let bits = if smep { CR4_SMEP } else { 0 }
        | if smap { CR4_SMAP } else { 0 }
        | if umip { CR4_UMIP } else { 0 };
    // SAFETY: the caller's contract; setting supported CR4 bits only adds restrictions on
    // supervisor accesses to user pages and on user-mode descriptor reads. The flags are not
    // preserved: `or` writes them.
    unsafe {
        asm!(
            "mov {tmp}, cr4",
            "or {tmp}, {bits}",
            "mov cr4, {tmp}",
            bits = in(reg) bits,
            tmp = out(reg) _,
            options(nomem, nostack),
        );
    }
    (smep, smap)
}

/// `movzx eax, byte ptr [rdi]` is the first instruction, so a fault has RIP = `read_byte`.
#[unsafe(naked)]
unsafe extern "sysv64" fn read_byte(_addr: u64) -> u64 {
    naked_asm!("movzx eax, byte ptr [rdi]", "mov rax, -1", "ret", "int3");
}

/// `mov byte ptr [rdi], sil` is the first instruction, so a fault has RIP = `write_byte`.
#[unsafe(naked)]
unsafe extern "sysv64" fn write_byte(_addr: u64, _value: u8) -> u64 {
    naked_asm!("mov byte ptr [rdi], sil", "mov rax, -1", "ret", "int3");
}

/// Jumps to `addr` with the caller's return address on the stack.
#[unsafe(naked)]
unsafe extern "sysv64" fn jump(_addr: u64) -> u64 {
    naked_asm!("jmp rdi");
}

/// Where a probe fault resumes: the stack holds the probe caller's return address and RAX
/// holds the error code, so `ret` returns the code to that caller.
#[unsafe(naked)]
extern "sysv64" fn probe_recover() {
    naked_asm!("ret", "int3");
}

/// The common entry of every vector: swaps the kernel's GS base in if the processor came
/// from ring 3 (the CS it pushed, at [rsp + 24]), then joins [`exception_saved`].
///
/// A program controls its own GS, so nothing here trusts GS until the swap, and `dispatch`
/// reads no CPU-local data for an NMI: an NMI that lands in the two instructions around
/// `swapgs` on the way in or out finds user mode's GS base, but CS says ring 0.
///
/// `lfence` on both paths: otherwise a mispredicted branch could run the GS-relative loads
/// below with the wrong GS base, the kernel's data reachable through a program's
/// (CVE-2019-1125). The join point has one `lfence`, so both paths reach it.
#[unsafe(naked)]
extern "sysv64" fn exception_common() {
    naked_asm!(
        "test byte ptr [rsp + 24], 3",
        "jz 2f",
        "swapgs",
        "2:",
        "lfence",
        "jmp {saved}",
        saved = sym exception_saved,
    );
}

/// Saves the general-purpose registers, calls [`dispatch`] with a pointer to the frame,
/// switches to the frame `dispatch` returns (the same one, or another thread's saved frame
/// with the same layout), restores its registers, drops the vector and error code, swaps
/// user mode's GS base back in if the frame returns to ring 3, and returns from the
/// interrupt into it.
///
/// On entry the stack holds, from the top: vector, error code, then the processor's frame.
/// The processor aligned RSP to 16 bytes before its 40-byte frame; with the 16 bytes of
/// error code and vector and 120 bytes of registers, RSP is 16-byte aligned at the `call`.
///
/// RFLAGS.AC is cleared if SMAP is on, whatever the interrupted code left in it: otherwise
/// a program could set AC and turn SMAP off for the kernel code an interrupt runs (an
/// interrupted user copy keeps its AC in the saved frame, and gets it back from IRETQ).
///
/// Once saved, every general-purpose register but RSP is zeroed before `dispatch` runs, so a
/// wrongly guessed path in the kernel has no value a program chose to work with (ADR-0017 Z2;
/// AMD's mitigation G-1). The system call's arguments are read from the frame.
#[unsafe(naked)]
extern "sysv64" fn exception_saved() {
    naked_asm!(
        "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
        "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
        // ADR-0017 Z2: nothing the interrupted code chose stays in a register. `dispatch`
        // reads the frame. RBP is cleared by a conditional move, not a branch a processor
        // could guess, and only for an entry from ring 3 (the saved CS, 144 bytes up): from
        // ring 0 it is the frame-pointer chain a backtrace follows.
        "xor eax, eax", "xor ebx, ebx", "xor ecx, ecx", "xor edx, edx", "xor esi, esi",
        "xor r8d, r8d", "xor r9d, r9d", "xor r10d, r10d", "xor r11d, r11d",
        "xor r12d, r12d", "xor r13d, r13d", "xor r14d, r14d", "xor r15d, r15d",
        "test byte ptr [rsp + 144], 3",
        "cmovnz rbp, rax",
        "mov rdi, rsp",
        "cld",
        "cmp byte ptr [rip + {smap}], 0",
        "je 3f",
        "clac",
        "3:",
        "call {dispatch}",
        // `dispatch` returns the saved frame to resume: this one, or another thread's.
        "mov rsp, rax",
        "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
        "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
        "add rsp, 16",
        "test byte ptr [rsp + 8], 3",
        "jz 4f",
        "swapgs",
        "4:",
        "iretq",
        "int3",
        smap = sym SMAP_ON,
        dispatch = sym dispatch,
    );
}

/// Entry stub of the local APIC timer interrupt: no error code, so push 0, then the vector.
#[unsafe(naked)]
extern "sysv64" fn timer_stub() {
    naked_asm!(
        "push 0",
        "push {vector}",
        "jmp {common}",
        vector = const TIMER_VECTOR,
        common = sym exception_common,
    );
}

/// Entry stubs of the scheduler's software interrupts and the TLB shootdown IPI: no error
/// code, so push 0.
macro_rules! soft_stub {
    ($name:ident, $vector:expr) => {
        #[unsafe(naked)]
        extern "sysv64" fn $name() {
            naked_asm!(
                "push 0",
                "push {vector}",
                "jmp {common}",
                vector = const $vector,
                common = sym exception_common,
            );
        }
    };
}
soft_stub!(yield_stub, YIELD_VECTOR);
soft_stub!(sleep_stub, SLEEP_VECTOR);
soft_stub!(exit_stub, EXIT_VECTOR);
soft_stub!(shootdown_stub, crate::tlb::SHOOTDOWN_VECTOR);
soft_stub!(block_stub, BLOCK_VECTOR);
soft_stub!(reschedule_stub, RESCHEDULE_VECTOR);
soft_stub!(device_stub, DEVICE_VECTOR);
soft_stub!(net_stub, NET_VECTOR);
soft_stub!(net_tx_stub, NET_TX_VECTOR);
soft_stub!(ioapic_stub, IOAPIC_VECTOR);

/// Entry stub of the spurious-interrupt vector.
#[unsafe(naked)]
extern "sysv64" fn spurious_stub() {
    naked_asm!(
        "push 0",
        "push {vector}",
        "jmp {common}",
        vector = const SPURIOUS_VECTOR,
        common = sym exception_common,
    );
}

/// One entry stub per vector: push a dummy error code where the processor pushes none, push
/// the vector, and join [`exception_common`].
macro_rules! stubs {
    ($($name:ident $vector:literal),* $(,)?) => {
        $(
            #[unsafe(naked)]
            extern "sysv64" fn $name() {
                naked_asm!(
                    ".if {has_error} == 0",
                    "push 0",
                    ".endif",
                    "push {vector}",
                    "jmp {common}",
                    has_error = const d::has_error_code($vector) as u8,
                    vector = const $vector,
                    common = sym exception_common,
                );
            }
        )*
        /// The 32 exception stubs, indexed by vector.
        static STUBS: [extern "sysv64" fn(); 32] = [$($name),*];
    };
}

stubs! {
    stub_0 0, stub_1 1, stub_2 2, stub_3 3, stub_4 4, stub_5 5, stub_6 6, stub_7 7,
    stub_8 8, stub_9 9, stub_10 10, stub_11 11, stub_12 12, stub_13 13, stub_14 14, stub_15 15,
    stub_16 16, stub_17 17, stub_18 18, stub_19 19, stub_20 20, stub_21 21, stub_22 22, stub_23 23,
    stub_24 24, stub_25 25, stub_26 26, stub_27 27, stub_28 28, stub_29 29, stub_30 30, stub_31 31,
}
