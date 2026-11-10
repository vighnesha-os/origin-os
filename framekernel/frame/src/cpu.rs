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
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::apic::{SPURIOUS_VECTOR, TIMER_VECTOR, X2APIC_EOI};
use crate::descriptors::{
    self as d, DOUBLE_FAULT_IST, GDT_ENTRIES, KERNEL_CODE, KERNEL_DATA, MACHINE_CHECK_IST, NMI_IST,
    TSS, TSS_SIZE,
};
use crate::trap::Switch;

/// Size of each interrupt stack (#DF, NMI, #MC, #DB).
const IST_STACK_SIZE: usize = 16 * 1024;
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

/// One interrupt stack and the page below it. That page is unmapped once the kernel's own
/// page tables are live (`smp::start`), so running off the end of the stack faults instead of
/// overwriting what lies below: another stack, the TSS or the GDT.
#[repr(C, align(4096))]
struct IstStack {
    guard: [u8; 4096],
    stack: [u8; IST_STACK_SIZE],
}

const ZERO_IST_STACK: IstStack = IstStack {
    guard: [0; 4096],
    stack: [0; IST_STACK_SIZE],
};

/// The operand of `lgdt` and `lidt`: a 16-bit limit followed by a 64-bit base.
#[repr(C, packed)]
struct TablePointer {
    limit: u16,
    base: u64,
}

/// One processor's GDT, TSS and interrupt stacks. The IDT is shared. The GDT and TSS have the
/// first page to themselves, and every interrupt stack starts on a page boundary.
#[repr(C, align(4096))]
pub struct CpuTables {
    gdt: Gdt,
    tss: TssImage,
    double_fault: IstStack,
    nmi: IstStack,
    machine_check: IstStack,
    debug: IstStack,
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
    double_fault: ZERO_IST_STACK,
    nmi: ZERO_IST_STACK,
    machine_check: ZERO_IST_STACK,
    debug: ZERO_IST_STACK,
};
static mut IDT: Idt = Idt([[0; 2]; 256]);

/// Unmaps the guard page below each of the four interrupt stacks in `tables` and returns
/// their addresses (`smp::guard_interrupt_stacks` probes each one).
///
/// # Safety
///
/// `tables` must point at a live `CpuTables` whose interrupt stacks and guard pages nothing is
/// using: an AP's tables before it starts, or the boot processor's (its stacks are above the
/// guards and have not been used down that far).
pub(crate) unsafe fn protect_interrupt_stacks(
    tables: *const CpuTables,
) -> Result<[u64; 4], &'static str> {
    let guards = interrupt_stack_guards(tables);
    crate::vm::make_guards(&guards).map_err(|_| "cannot unmap the guard pages")?;
    Ok(guards)
}

/// The guard pages of the four interrupt stacks in `tables`.
pub(crate) fn interrupt_stack_guards(tables: *const CpuTables) -> [u64; 4] {
    let base = tables.addr() as u64;
    [
        base + core::mem::offset_of!(CpuTables, double_fault.guard) as u64,
        base + core::mem::offset_of!(CpuTables, nmi.guard) as u64,
        base + core::mem::offset_of!(CpuTables, machine_check.guard) as u64,
        base + core::mem::offset_of!(CpuTables, debug.guard) as u64,
    ]
}

/// The boot processor's tables, for [`interrupt_stack_guards`].
pub(crate) fn bsp_tables() -> *const CpuTables {
    &raw const BSP_TABLES
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
        let ist = match vector {
            1 => DEBUG_IST,
            2 => NMI_IST,
            8 => DOUBLE_FAULT_IST,
            18 => MACHINE_CHECK_IST,
            _ => 0,
        };
        *gate = d::interrupt_gate(*stub as usize as u64, KERNEL_CODE, ist);
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
    // SAFETY: `BSP_TABLES` is used only by the boot processor, and only from here on.
    unsafe { load_tables(&raw mut BSP_TABLES, CpuIndex::BOOT) };
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
/// makes it CPU `index` (if its APIC ID is the one [`assign`]ed to `index`).
///
/// # Safety
///
/// Call once per application processor, at CPL 0 with interrupts disabled, after [`init`]
/// ran on the boot processor. `tables` must be writable, live for ever, and used by no
/// other processor; no other processor may be CPU `index`.
pub unsafe fn init_ap(tables: *mut CpuTables, index: CpuIndex) {
    // SAFETY: the caller's contract is `load_tables`'s.
    unsafe { load_tables(tables, index) };
}

/// Writes the GDT and TSS in `tables` and loads them, the segment registers and the IDT.
///
/// # Safety
///
/// As for [`init_ap`]; for the boot processor, `tables` is `BSP_TABLES` and `index` is
/// [`CpuIndex::BOOT`].
unsafe fn load_tables(tables: *mut CpuTables, index: CpuIndex) {
    // SAFETY: the caller's contract: `tables` is valid, writable and this processor's own.
    // Projecting to a field creates no reference.
    let stack = unsafe { &raw mut (*tables).double_fault.stack };
    // SAFETY: as above.
    let nmi = unsafe { &raw mut (*tables).nmi.stack };
    // SAFETY: as above.
    let mc = unsafe { &raw mut (*tables).machine_check.stack };
    // SAFETY: as above.
    let debug = unsafe { &raw mut (*tables).debug.stack };
    // SAFETY: as above.
    let tss_field = unsafe { &raw mut (*tables).tss };
    // SAFETY: as above.
    let gdt_ptr_raw = unsafe { &raw mut (*tables).gdt };
    let top = |p: usize| (p + IST_STACK_SIZE) as u64;
    let mut tss_image = TssImage(d::tss_image([
        top(stack.addr()),
        top(nmi.addr()),
        top(mc.addr()),
    ]));
    let debug_top = top(debug.addr());
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
}

/// Processors the kernel indexes: every per-CPU array, the CPU-local areas included, has
/// this many slots (`framekernel::cpumap`).
pub use crate::limits::MAX_CPUS;
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

    /// Index `index`, if an array has that slot (below [`MAX_CPUS`]).
    pub const fn new(index: usize) -> Option<Self> {
        if index < MAX_CPUS {
            Some(Self(index))
        } else {
            None
        }
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
static APIC_IDS: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(UNASSIGNED) }; MAX_CPUS];

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
struct CpuLocal {
    /// This processor's CPU index (offset 0, read by [`this_cpu`]).
    id: AtomicU64,
    /// Address of this processor's TSS (offset 8, used by [`set_kernel_stack`]).
    tss: AtomicU64,
    /// The running thread's kernel stack top (offset 16), the stack `syscall_entry`
    /// switches to; kept equal to TSS.RSP0.
    kernel_stack: AtomicU64,
    /// Scratch for the user RSP during `syscall_entry` (offset 24).
    user_rsp: AtomicU64,
    /// The instruction at which this CPU's armed probe expects a fault, or 0.
    probe_rip: AtomicU64,
    /// CR2 of this CPU's last probe fault.
    probe_address: AtomicU64,
}

static CPU_LOCAL: [CpuLocal; MAX_CPUS] = [const {
    CpuLocal {
        id: AtomicU64::new(0),
        tss: AtomicU64::new(0),
        kernel_stack: AtomicU64::new(0),
        user_rsp: AtomicU64::new(0),
        probe_rip: AtomicU64::new(0),
        probe_address: AtomicU64::new(0),
    }
}; MAX_CPUS];
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

/// Each CPU index whose processor passed [`cpu_state_holds`] on itself.
static CPU_STATE_HOLDS: [core::sync::atomic::AtomicBool; MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; MAX_CPUS];

/// Records whether this processor's control state holds, after `enable_simd`. The boot
/// processor and every AP do it on themselves; an AP before its handshake, which orders the
/// mark before the boot processor sees it online.
pub fn record_cpu_state() {
    if let Some(holds) = CPU_STATE_HOLDS.get(this_cpu().get()) {
        holds.store(cpu_state_holds(), Ordering::Relaxed);
    }
}

/// How many of `cpus` recorded a control state that holds.
pub fn cpu_states_holding(cpus: &[CpuIndex]) -> usize {
    cpus.iter()
        .filter(|c| {
            CPU_STATE_HOLDS
                .get(c.get())
                .is_some_and(|h| h.load(Ordering::Relaxed))
        })
        .count()
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

/// This processor's CPU-local area.
fn local() -> &'static CpuLocal {
    CPU_LOCAL.get(this_cpu().0).unwrap_or(&OUT_OF_RANGE)
}

/// Sets this processor's TSS.RSP0 to `stack`'s top: the stack an interrupt or exception from
/// user mode switches to. `stack` is the kernel stack of the thread about to run in user mode;
/// taking it by reference means a stale or invented value cannot be installed.
pub fn set_kernel_stack(stack: &crate::stack::GuardedStack) {
    let top = stack.top();
    let local = this_cpu().0;
    let Some(tss) = CPU_LOCAL.get(local).map(|l| l.tss.load(Ordering::Relaxed)) else {
        return;
    };
    if tss == 0 {
        return;
    }
    let field = ptr::with_exposed_provenance_mut::<u32>(tss as usize + d::TSS_RSP0_OFFSET);
    if let Some(l) = CPU_LOCAL.get(local) {
        l.kernel_stack.store(top, Ordering::Relaxed);
    }
    // SAFETY: `tss` is this processor's TSS (recorded by `init_cpu_local`), live for ever
    // and written only by this processor; RSP0 is two `u32` words at byte 4, 4-aligned.
    unsafe { field.write_volatile(top as u32) };
    // SAFETY: as above, the high word.
    unsafe { field.wrapping_add(1).write_volatile((top >> 32) as u32) };
}

/// Records that this processor is running a context with no kernel stack of its own (the
/// boot or idle flow): nothing is on any freed stack's account (`stack_in_use`).
pub fn clear_kernel_stack() {
    if let Some(l) = CPU_LOCAL.get(this_cpu().0) {
        l.kernel_stack.store(0, Ordering::Relaxed);
    }
}

/// How many thread frames one processor can have saved at once: the run queue's capacity
/// (`RunQueue` holds 16 contexts) and some.
const SAVED_FRAME_SLOTS: usize = 32;
/// Each processor's saved thread frames: addresses the trap entry saved (a thread preempted,
/// yielding, sleeping or blocked) or a new thread's first frame. Only these are ever
/// resumed, so a wrong answer from the scheduler above cannot make the trap entry load anything
/// else. 0 is an empty slot.
static SAVED_FRAMES: [[AtomicU64; SAVED_FRAME_SLOTS]; MAX_CPUS] =
    [const { [const { AtomicU64::new(0) }; SAVED_FRAME_SLOTS] }; MAX_CPUS];

/// Records `frame` as a saved frame of processor `cpu`: a thread's first frame, or one the
/// trap entry saved. `false` if the table is full (it cannot be: see the slot count).
pub(crate) fn record_saved_frame(cpu: CpuIndex, frame: u64) -> bool {
    SAVED_FRAMES.get(cpu.get()).is_some_and(|table| {
        table.iter().any(|slot| {
            slot.compare_exchange(0, frame, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        })
    })
}

/// Consumes `frame` from this processor's saved frames; `false` if it is not one of them.
fn take_saved_frame(frame: u64) -> bool {
    let me = this_cpu().get();
    SAVED_FRAMES.get(me).is_some_and(|table| {
        table.iter().any(|slot| {
            slot.compare_exchange(frame, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        })
    })
}

/// What the trap entry resumes after a scheduler upcall: `next`, the frame the scheduler
/// answered for the interrupted context at `here`. Unless they are the same, `next` must be one
/// of the frames this processor saved (the scheduler can name no other), and `here` becomes
/// one, unless the interrupted thread is ending (`exiting`).
fn resume(here: u64, next: u64, exiting: bool) -> u64 {
    if next == here {
        return here;
    }
    if !take_saved_frame(next) {
        panic!("the scheduler returned a frame the kernel never saved: {next:#x}");
    }
    if !exiting && !record_saved_frame(this_cpu(), here) {
        panic!("too many saved thread frames on this processor");
    }
    next
}

/// Whether a processor is using the kernel stack `[start, end)`: it is some processor's current
/// kernel stack, or a saved thread frame is on it. Only a stack that no processor is using
/// can be freed.
pub(crate) fn stack_in_use(start: u64, end: u64, top: u64) -> bool {
    let used = |value: u64| value >= start && value < end;
    let here = {
        let rsp: u64;
        // SAFETY: reads the stack pointer; no memory, no stack, flags preserved.
        unsafe { asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags)) };
        rsp
    };
    used(here)
        // `kernel_stack` holds the top of the stack the processor runs on: this stack's top is
        // one past its end, so compare it exactly (another stack's top can equal our guard
        // page's address).
        || CPU_LOCAL
            .iter()
            .any(|l| l.kernel_stack.load(Ordering::Relaxed) == top)
        || SAVED_FRAMES
            .iter()
            .flatten()
            .any(|f| used(f.load(Ordering::Acquire)))
}

/// This processor's CPU index: one GS-relative load from its CPU-local area. On a
/// processor without one ([`OUT_OF_RANGE`]) it is an index no array has.
pub fn this_cpu() -> CpuIndex {
    let id: u64;
    // SAFETY: `load_tables` set GS's base to a live `CpuLocal` on every processor before
    // any code that calls this runs, and offset 0 is its `id`, a `u64`.
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
    // SAFETY: only the address of the static's field is taken; no reference is made.
    let low = unsafe { &raw const BSP_TABLES.nmi.stack }.addr() as u64;
    (low, low + IST_STACK_SIZE as u64)
}

/// The frame of the last user-mode #DB, for the boot proof that it arrives on its own stack.
static DEBUG_FRAME: AtomicU64 = AtomicU64::new(0);

/// Where the last #DB in user mode saved its frame.
pub fn last_debug_frame() -> u64 {
    DEBUG_FRAME.load(Ordering::SeqCst)
}

/// The boot processor's #DB stack, as `[low, high)`.
pub fn bsp_debug_stack() -> (u64, u64) {
    // SAFETY: only the address of the static's field is taken; no reference is made.
    let low = unsafe { &raw const BSP_TABLES.debug.stack }.addr() as u64;
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
    // the named XMM registers from general registers.
    unsafe {
        asm!(
            "movq xmm0, {0}",
            "movq xmm3, {1}",
            "movq xmm7, {2}",
            "movq xmm15, {3}",
            in(reg) values[0], in(reg) values[1], in(reg) values[2], in(reg) values[3],
            out("xmm0") _, out("xmm3") _, out("xmm7") _, out("xmm15") _,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Reads XMM0, XMM3, XMM7 and XMM15 (the SIMD state the boot proof checks).
pub fn xmm_read() -> [u64; 4] {
    let (a, b, c, d): (u64, u64, u64, u64);
    // SAFETY: reads the named XMM registers; no memory, no stack.
    unsafe {
        asm!(
            "movq {0}, xmm0",
            "movq {1}, xmm3",
            "movq {2}, xmm7",
            "movq {3}, xmm15",
            out(reg) a, out(reg) b, out(reg) c, out(reg) d,
            options(nomem, nostack, preserves_flags),
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
            resume(here, (h.tick)(here, &Switch::new()), false)
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
        return match irq::run(vector) {
            Action::Resume => here,
            Action::Reschedule => hooks.map_or(here, |h| {
                resume(here, (h.reschedule)(here, &Switch::new()), false)
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
            resume(here, (h.reschedule)(here, &Switch::new()), false)
        });
    }
    // Another processor changed a kernel mapping: invalidate it here, then acknowledge.
    if frame.vector == u64::from(crate::tlb::SHOOTDOWN_VECTOR) {
        crate::tlb::interrupted();
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
            resume(here, (h.yield_now)(here, &Switch::new()), false)
        });
    }
    if frame.vector == u64::from(SLEEP_VECTOR) {
        return hooks.map_or(here, |h| {
            resume(here, (h.sleep)(here, frame.rdi, &Switch::new()), false)
        });
    }
    if frame.vector == u64::from(EXIT_VECTOR) {
        return hooks.map_or(here, |h| resume(here, (h.exit)(here, &Switch::new()), true));
    }
    if frame.vector == u64::from(BLOCK_VECTOR) {
        return hooks.map_or(here, |h| {
            resume(here, (h.block)(here, frame.rdi, &Switch::new()), false)
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
            crate::trap::SyscallOutcome::ThreadExit => {
                resume(here, (hooks.exit)(here, &Switch::new()), true)
            }
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
        return resume(here, (hooks.exit)(here, &Switch::new()), true);
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
        let armed = local().probe_rip.load(Ordering::SeqCst);
        if armed != 0 && frame.rip == armed {
            local().probe_address.store(read_cr2(), Ordering::SeqCst);
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

/// One `RDRAND` attempt: the 64-bit value on success (carry set), or `None` on a transient
/// failure a retry may satisfy. The caller must have confirmed `Features::rdrand`.
pub fn rdrand() -> Option<u64> {
    let (value, ok): (u64, u8);
    // SAFETY: RDRAND writes only its destination register and the carry flag, reads no
    // memory, and is defined on every long-mode processor that reports it in CPUID (which
    // the caller checks). It is not `pure`: each call returns fresh bits.
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

/// One `RDSEED` attempt: a full-entropy 64-bit value on success, or `None`. `RDSEED` draws
/// directly from the entropy source and fails more often than `RDRAND` under load, so it
/// needs the same bounded retry. The caller must have confirmed `Features::rdseed`.
pub fn rdseed() -> Option<u64> {
    let (value, ok): (u64, u8);
    // SAFETY: as `rdrand`; RDSEED has the same register/flag-only effect and CPUID gate.
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
    crate::unwind::walk(
        rbp,
        BACKTRACE_MAX_FRAME,
        |addr| {
            if addr < base || addr > base.saturating_add(BACKTRACE_SPAN) || addr & 7 != 0 {
                return None;
            }
            let p = ptr::with_exposed_provenance::<u64>(addr as usize);
            // SAFETY: `addr` is 8-aligned and within `BACKTRACE_SPAN` bytes above the
            // starting frame pointer, so it lies in the running stack, which is mapped.
            Some(unsafe { p.read_volatile() })
        },
        out,
    )
}

/// The TSC, read only once every earlier instruction has executed, so a reading cannot be
/// taken before the lock that orders it is held: RDTSCP (Intel SDM Vol. 2B, RDTSCP; AMD
/// APM Vol. 3) if `use_rdtscp` and this processor has it, else LFENCE then RDTSC.
pub fn ordered_tsc(use_rdtscp: bool) -> u64 {
    let has_rdtscp = use_rdtscp
        && cpuid(0x8000_0000)[0] >= 0x8000_0001
        && cpuid(0x8000_0001)[3] & (1 << 27) != 0;
    let (low, high): (u32, u32);
    if has_rdtscp {
        // SAFETY: RDTSCP exists on this processor (just checked); it writes EDX:EAX and ECX
        // (IA32_TSC_AUX, unused here) and reads no memory.
        unsafe {
            asm!("rdtscp", out("eax") low, out("edx") high, out("ecx") _, options(nomem, nostack, preserves_flags));
        }
    } else {
        // SAFETY: LFENCE and RDTSC exist on every x86-64 processor and read no memory.
        unsafe {
            asm!("lfence", "rdtsc", out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
        }
    }
    (u64::from(high) << 32) | u64::from(low)
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

/// What a probe access did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Arms the probe for a fault at `fault_rip`, runs `access`, and disarms it.
fn run_probe(fault_rip: u64, access: impl FnOnce() -> u64) -> Probe {
    // Per CPU: probes and user copies on different processors never disarm each other.
    let here = local();
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
    naked_asm!("rep movsb", "mov rax, -1", "ret");
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
static PAT_READY: [core::sync::atomic::AtomicBool; MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; MAX_CPUS];

/// Checks that this processor's PAT has entry 0 write-back (06h) and entry 3 uncached
/// (00h), the memory types `paging::Cache` selects (Intel SDM Vol. 3A §11.12.2,
/// Table 11-10), and marks this processor's CPU index if so. These are the architectural
/// reset values; a firmware that changed them would make uncached mappings cacheable, so the
/// kernel then refuses to map device registers rather than reprogramming caches it cannot
/// see. Returns the PAT.
pub fn check_pat() -> u64 {
    // SAFETY: CPL 0; IA32_PAT exists on every x86-64 processor (CPUID 1 EDX bit 16) and
    // reading it has no side effect.
    let pat = unsafe { rdmsr(IA32_PAT) };
    if pat & 0xFF == 0x06
        && (pat >> 24) & 0xFF == 0x00
        && let Some(ready) = PAT_READY.get(this_cpu().get())
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

/// Whether this processor's SIMD state is managed with XSAVE (else FXSAVE).
static USE_XSAVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// The XSAVE/FXSAVE area size the kernel allocates per thread, once known.
static FPU_AREA_SIZE: AtomicU64 = AtomicU64::new(crate::features::FXSAVE_SIZE as u64);

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

/// Enables SSE (and AVX where present) on this processor and sets the per-thread FPU-area
/// size from `features`. Returns the XCR0 value in force (0 if XSAVE is not used).
///
/// The kernel's own code is soft-float (the `x86_64-unknown-none` ABI uses no XMM), so this
/// adds a capability for explicit SIMD and for user programs; it does not change how kernel
/// code is generated.
///
/// # Safety
///
/// CPL 0. Must run once per processor before any code on it uses SSE or XSAVE.
pub unsafe fn enable_simd(id: &crate::features::Cpuid) -> u64 {
    use crate::features::{Features, wanted_xcr0, xsave_area_size};
    let features = Features::decode(id);
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
    if features.xsave {
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
    let mut xcr0 = 0;
    if features.xsave {
        xcr0 = wanted_xcr0(&features);
        // SAFETY: CR4.OSXSAVE was just set; `xcr0` has x87 and SSE (required) and AVX only
        // when the CPU reports it, so it is a legal XCR0.
        unsafe { xsetbv(0, xcr0) };
        USE_XSAVE.store(true, core::sync::atomic::Ordering::Relaxed);
        if let Some(size) = xsave_area_size(id) {
            // The area must hold every enabled component; take the maximum CPUID reports.
            FPU_AREA_SIZE.store(size as u64, core::sync::atomic::Ordering::Relaxed);
        }
        // SAFETY: OSXSAVE is set.
        let _ = unsafe { xgetbv(0) };
    }
    xcr0
}

/// The per-thread FPU save area size (bytes) and alignment (64).
pub fn fpu_area_size() -> usize {
    FPU_AREA_SIZE.load(core::sync::atomic::Ordering::Relaxed) as usize
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
        // SAFETY: the caller's contract; the area is large enough (CPUID's maximum).
        unsafe {
            asm!("xsave [{a}]", a = in(reg) area, in("eax") u32::MAX, in("edx") u32::MAX, options(nostack, preserves_flags));
        }
    } else {
        // SAFETY: as above; FXSAVE needs 512 bytes, 16-byte aligned.
        unsafe {
            asm!("fxsave [{a}]", a = in(reg) area, options(nostack, preserves_flags));
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
        // SAFETY: the caller's contract; the area holds a valid XSAVE image.
        unsafe {
            asm!("xrstor [{a}]", a = in(reg) area, in("eax") u32::MAX, in("edx") u32::MAX, options(nostack, preserves_flags));
        }
    } else {
        // SAFETY: as above.
        unsafe {
            asm!("fxrstor [{a}]", a = in(reg) area, options(nostack, preserves_flags));
        }
    }
}

/// Enables SMEP and SMAP on this processor, where supported; returns which were enabled.
///
/// # Safety
///
/// CPL 0. The kernel must not rely on executing or accessing user pages from supervisor
/// mode (it has none; a future user-copy path must set RFLAGS.AC around its accesses).
pub unsafe fn enable_smep_smap() -> (bool, bool) {
    let (smep, smap) = smep_smap_supported();
    // The boot processor's answer decides what the kernel assumes: an AP that lacks SMAP
    // refuses to start (`smp::ap_entry`) and must not turn it off for everyone else.
    if this_cpu() == CpuIndex::BOOT {
        SMAP_ON.store(smap, Ordering::Relaxed);
    }
    // UMIP (CPUID.7.0:ECX bit 2): `sidt` from ring 3 would otherwise reveal the kernel's
    // descriptor-table addresses, and with them the KASLR slide (ADR-0004 H3).
    let umip = cpuid(0)[0] >= 7 && cpuid(7)[2] & (1 << 2) != 0;
    if umip && this_cpu() == CpuIndex::BOOT {
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
    naked_asm!("movzx eax, byte ptr [rdi]", "mov rax, -1", "ret");
}

/// `mov byte ptr [rdi], sil` is the first instruction, so a fault has RIP = `write_byte`.
#[unsafe(naked)]
unsafe extern "sysv64" fn write_byte(_addr: u64, _value: u8) -> u64 {
    naked_asm!("mov byte ptr [rdi], sil", "mov rax, -1", "ret");
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
    naked_asm!("ret");
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
#[unsafe(naked)]
extern "sysv64" fn exception_saved() {
    naked_asm!(
        "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
        "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
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
