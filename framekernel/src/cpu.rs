//! The kernel's own GDT, TSS and IDT, and CPU exception dispatch: the first M2 step
//! (CLAUDE.md §24; Intel SDM Vol. 3A §3.4, §6.10–§6.14, §7.2, §7.7).
//!
//! The descriptor encodings live in the library (`framekernel::descriptors`) and are
//! unit-tested there; this module only places them in memory and loads them. Exception
//! entry uses one naked stub per vector, which normalises the frame (pushing a dummy error
//! code where the processor does not push one, then the vector number), and a common stub
//! that saves the general-purpose registers and calls [`dispatch`] with System V alignment.
//!
//! It also switches to the kernel's own page tables ([`load_page_tables`]) and provides
//! *probes*: single deliberate accesses that report a page fault instead of taking it, which
//! prove on the real processor that the page protections hold.

use core::arch::{asm, naked_asm};
use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use framekernel::apic::{SPURIOUS_VECTOR, TIMER_VECTOR, X2APIC_EOI};
use framekernel::descriptors::{
    self as d, DOUBLE_FAULT_IST, GDT_ENTRIES, KERNEL_CODE, KERNEL_DATA, TSS, TSS_SIZE,
};
use framekernel::serial::Uart16550;

/// Size of the double-fault stack.
const DOUBLE_FAULT_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct Gdt([u64; GDT_ENTRIES]);

#[repr(C, align(16))]
struct TssImage([u32; 26]);

#[repr(C, align(4096))]
struct Idt([[u64; 2]; 256]);

#[repr(C, align(16))]
struct Stack([u8; DOUBLE_FAULT_STACK_SIZE]);

/// The operand of `lgdt` and `lidt`: a 16-bit limit followed by a 64-bit base.
#[repr(C, packed)]
struct TablePointer {
    limit: u16,
    base: u64,
}

/// One processor's GDT, TSS and double-fault stack. The IDT is shared.
#[repr(C, align(16))]
pub(crate) struct CpuTables {
    gdt: Gdt,
    tss: TssImage,
    double_fault_stack: Stack,
}

/// The boot processor's tables. Application processors get theirs from the heap.
static mut BSP_TABLES: CpuTables = CpuTables {
    gdt: Gdt([0; GDT_ENTRIES]),
    tss: TssImage([0; 26]),
    double_fault_stack: Stack([0; DOUBLE_FAULT_STACK_SIZE]),
};
static mut IDT: Idt = Idt([[0; 2]; 256]);

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
pub(crate) unsafe fn init() {
    let mut idt = Idt([[0; 2]; 256]);
    for (vector, (gate, stub)) in idt.0.iter_mut().zip(STUBS.iter()).enumerate() {
        let ist = if vector == 8 { DOUBLE_FAULT_IST } else { 0 };
        *gate = d::interrupt_gate(*stub as usize as u64, KERNEL_CODE, ist);
    }
    let interrupts: [(u8, extern "sysv64" fn()); 5] = [
        (TIMER_VECTOR, timer_stub),
        (SPURIOUS_VECTOR, spurious_stub),
        (YIELD_VECTOR, yield_stub),
        (SLEEP_VECTOR, sleep_stub),
        (EXIT_VECTOR, exit_stub),
    ];
    for (vector, stub) in interrupts {
        if let Some(gate) = idt.0.get_mut(usize::from(vector)) {
            *gate = d::interrupt_gate(stub as usize as u64, KERNEL_CODE, 0);
        }
    }
    // SAFETY: the IDT is written once, here, before any `lidt` points a processor at it
    // (the caller's contract: single-threaded, first).
    unsafe { (&raw mut IDT).write(idt) };
    // SAFETY: `BSP_TABLES` is used only by the boot processor, and only from here on.
    unsafe { load_tables(&raw mut BSP_TABLES) };
}

/// Loads an application processor's own GDT and TSS from `tables`, and the shared IDT.
///
/// # Safety
///
/// Call once per application processor, at CPL 0 with interrupts disabled, after [`init`]
/// ran on the boot processor. `tables` must be writable, live for ever, and used by no
/// other processor.
pub(crate) unsafe fn init_ap(tables: *mut CpuTables) {
    // SAFETY: the caller's contract is `load_tables`'s.
    unsafe { load_tables(tables) };
}

/// Writes the GDT and TSS in `tables` and loads them, the segment registers and the IDT.
///
/// # Safety
///
/// As for [`init_ap`]; for the boot processor, `tables` is `BSP_TABLES`.
unsafe fn load_tables(tables: *mut CpuTables) {
    // SAFETY: the caller's contract: `tables` is valid, writable and this processor's own.
    // Projecting to a field creates no reference.
    let stack = unsafe { &raw mut (*tables).double_fault_stack };
    // SAFETY: as above.
    let tss_field = unsafe { &raw mut (*tables).tss };
    // SAFETY: as above.
    let gdt_ptr_raw = unsafe { &raw mut (*tables).gdt };
    let stack_top = stack.addr() + DOUBLE_FAULT_STACK_SIZE;
    let tss_image = TssImage(d::tss_image(stack_top as u64));
    // SAFETY: as above; the TSS is written before `ltr` loads it.
    unsafe { tss_field.write(tss_image) };
    let tss = d::tss_descriptor(tss_field.addr() as u64);
    let gdt = Gdt([
        0,
        d::kernel_code_segment(),
        d::kernel_data_segment(),
        tss[0],
        tss[1],
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
            tmp = lateout(reg) _,
        );
    }
    // SAFETY: `idt_ptr` describes the IDT `init` wrote; every present gate points at a stub
    // in the kernel code segment just loaded.
    unsafe { asm!("lidt [{idt}]", idt = in(reg) &raw const idt_ptr, options(nostack)) };
    let _ = TSS_SIZE; // the limit is encoded by `tss_descriptor`
    // GS was just reloaded (base 0): point it at this processor's CPU-local area.
    init_cpu_local();
}

/// Processors with a CPU-local area, indexed by x2APIC ID.
const MAX_LOCAL: usize = 64;
/// IA32_GS_BASE: the base of GS in 64-bit mode.
const GS_BASE: u32 = 0xC000_0101;

/// One processor's CPU-local area, reached through GS. A cache line each, so processors
/// never share one.
#[repr(C, align(64))]
struct CpuLocal {
    /// This processor's x2APIC ID (offset 0, read by [`cpu_id`]).
    id: AtomicU64,
}

static CPU_LOCAL: [CpuLocal; MAX_LOCAL] = [const {
    CpuLocal {
        id: AtomicU64::new(0),
    }
}; MAX_LOCAL];
/// The area of a processor whose ID is too large for [`CPU_LOCAL`]: its ID reads as
/// `u64::MAX`, which no queue accepts, so it never schedules.
static OUT_OF_RANGE: CpuLocal = CpuLocal {
    id: AtomicU64::new(u64::MAX),
};

/// This processor's x2APIC ID from CPUID (leaf 0Bh EDX, else leaf 1 EBX bits 31:24), which
/// does not depend on the local APIC's mode.
pub(crate) fn cpuid_apic_id() -> u32 {
    if cpuid(0)[0] >= 0xB {
        cpuid(0xB)[3]
    } else {
        cpuid(1)[1] >> 24
    }
}

/// Points IA32_GS_BASE at this processor's [`CpuLocal`].
fn init_cpu_local() {
    let id = cpuid_apic_id() as usize;
    let local = match CPU_LOCAL.get(id) {
        Some(local) => {
            local.id.store(id as u64, Ordering::Relaxed);
            local
        }
        None => &OUT_OF_RANGE,
    };
    // SAFETY: CPL 0 (every caller loads descriptor tables); IA32_GS_BASE exists on every
    // x86-64 processor and only sets GS's base, which only `cpu_id` uses.
    unsafe { wrmsr(GS_BASE, ptr::from_ref(local).addr() as u64) };
}

/// This processor's x2APIC ID: one GS-relative load from its CPU-local area.
pub(crate) fn cpu_id() -> u64 {
    let id: u64;
    // SAFETY: `load_tables` set GS's base to a live `CpuLocal` on every processor before
    // any code that calls this runs, and offset 0 is its `id`, a `u64`.
    unsafe { asm!("mov {}, gs:[0]", out(reg) id, options(nostack, readonly, preserves_flags)) };
    id
}

/// Software interrupt a kernel thread raises to yield the processor.
pub(crate) const YIELD_VECTOR: u8 = 0x81;
/// Software interrupt to sleep for RDI timer ticks.
pub(crate) const SLEEP_VECTOR: u8 = 0x82;
/// Software interrupt to end the running thread.
pub(crate) const EXIT_VECTOR: u8 = 0x83;

/// Local APIC timer interrupts taken since boot.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Local APIC timer interrupts taken since boot.
pub(crate) fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Executes `int3`; the breakpoint handler reports it and returns here.
pub(crate) fn breakpoint() {
    // SAFETY: `int3` raises #BP, whose gate `init` installed; the handler returns with
    // `iretq` to the instruction after `int3`, preserving every register.
    unsafe { asm!("int3") };
}

/// Called by [`exception_common`] with the saved frame.
extern "sysv64" fn dispatch(frame: &mut ExceptionFrame) -> u64 {
    // The frame to resume: this one, unless the scheduler switches to another thread.
    let here = ptr::from_mut(frame).addr() as u64;
    // Device interrupts: the timer counts and acknowledges; a spurious interrupt needs no
    // EOI (SDM Vol. 3A, spurious interrupt).
    if frame.vector == u64::from(TIMER_VECTOR) {
        TICKS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: x2APIC mode is enabled before the timer is unmasked (`timer::init`), and
        // writing 0 to the EOI register only acknowledges the interrupt in service.
        unsafe { wrmsr(X2APIC_EOI, 0) };
        return crate::sched::on_tick(here);
    }
    if frame.vector == u64::from(SPURIOUS_VECTOR) {
        return here;
    }
    // Scheduler requests from kernel threads (`sched::yield_now`, `sleep`, `exit`).
    if frame.vector == u64::from(YIELD_VECTOR) {
        return crate::sched::on_yield(here);
    }
    if frame.vector == u64::from(SLEEP_VECTOR) {
        return crate::sched::on_sleep(here, frame.rdi);
    }
    if frame.vector == u64::from(EXIT_VECTOR) {
        return crate::sched::on_exit(here);
    }
    // A page fault at the exact instruction a probe armed: record it and resume at
    // `probe_recover`, which returns the error code to the probe's caller.
    if frame.vector == 14 {
        let armed = PROBE_RIP.load(Ordering::SeqCst);
        if armed != 0 && frame.rip == armed {
            PROBE_ADDRESS.store(read_cr2(), Ordering::SeqCst);
            frame.rax = frame.error_code;
            frame.rip = (probe_recover as *const ()).addr() as u64;
            return here;
        }
    }
    // SAFETY: COM1 was programmed by `_start` before `init` installed any gate, and only
    // this processor runs; an exception interrupts `_start` on that same processor.
    let mut com1 = Uart16550::new(unsafe { crate::Com1::new() });
    let vector = frame.vector as u8;
    let _ = writeln!(
        com1,
        "origin-kernel: exception {} ({}) at rip {:#x}, error code {:#x}",
        vector,
        d::exception_name(vector),
        frame.rip,
        frame.error_code
    );
    if vector == 3 {
        let _ = writeln!(com1, "origin-kernel: breakpoint handled; resuming");
        return here;
    }
    let _ = writeln!(
        com1,
        "origin-kernel: rsp {:#x} rflags {:#x} cs {:#x} rax {:#x} rbx {:#x} rcx {:#x} rdx {:#x}",
        frame.rsp, frame.rflags, frame.cs, frame.rax, frame.rbx, frame.rcx, frame.rdx
    );
    let _ = (
        frame.rsi, frame.rdi, frame.rbp, frame.r8, frame.r9, frame.r10, frame.r11,
    );
    let _ = (frame.r12, frame.r13, frame.r14, frame.r15, frame.ss);
    crate::halt()
}

/// CR2: the linear address of the last page fault.
fn read_cr2() -> u64 {
    let value: u64;
    // SAFETY: reading CR2 at CPL 0 has no side effects.
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// CPUID `leaf`, sub-leaf 0: `[eax, ebx, ecx, edx]`.
pub(crate) fn cpuid(leaf: u32) -> [u32; 4] {
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

/// Whether the processor supports the execute-disable bit (CPUID 8000_0001h EDX bit 20).
pub(crate) fn nx_supported() -> bool {
    cpuid(0x8000_0000)[0] >= 0x8000_0001 && cpuid(0x8000_0001)[3] & (1 << 20) != 0
}

/// Whether the processor supports x2APIC mode (CPUID 1 ECX bit 21).
pub(crate) fn x2apic_supported() -> bool {
    cpuid(1)[2] & (1 << 21) != 0
}

/// Reads model-specific register `msr`.
///
/// # Safety
///
/// CPL 0, and `msr` must exist on this processor (else #GP) and have no read side effects
/// this code does not expect.
pub(crate) unsafe fn rdmsr(msr: u32) -> u64 {
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
pub(crate) unsafe fn wrmsr(msr: u32, value: u64) {
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

/// The time-stamp counter.
pub(crate) fn rdtsc() -> u64 {
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
pub(crate) fn interrupt_window() {
    // SAFETY: CPL 0. Interrupts are recognized after the instruction that follows STI, so
    // a pending one is taken after the NOP and before CLI. Every vector that can arrive has
    // a gate (the timer, the spurious vector and the exceptions), and the 8259 PICs are
    // masked.
    unsafe { asm!("sti", "nop", "cli", options(nomem, nostack)) };
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
pub(crate) unsafe fn load_page_tables(root: u64) {
    // SAFETY: the caller's contract. EFER is read and written back with only NXE added
    // (NX is supported, so the bit is defined). WP only restricts supervisor writes to
    // pages already marked read-only. Loading CR3 switches to tables the caller vouches
    // for and flushes every non-global TLB entry.
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
            options(nostack, preserves_flags),
        );
    }
}

/// The instruction address at which an armed probe expects a page fault, or 0.
static PROBE_RIP: AtomicU64 = AtomicU64::new(0);
/// CR2 recorded by the last probe fault.
static PROBE_ADDRESS: AtomicU64 = AtomicU64::new(0);
/// What the probe functions return when the access completed (no #PF error code is this).
const COMPLETED: u64 = u64::MAX;

/// `mov rax, -1; ret`: executed only if NX fails to stop the fetch, in which case the jump
/// probe returns [`COMPLETED`]. It is writable data, so it must never be executable.
static mut PROBE_CODE: [u8; 8] = PROBE_RETURN;

/// What a probe access did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Probe {
    /// The access completed: the page allowed it.
    Completed,
    /// It raised #PF with this error code at this linear address (CR2).
    Faulted {
        /// The #PF error code (SDM Vol. 3A §4.7).
        error: u64,
        /// CR2.
        address: u64,
    },
}

/// Arms the probe for a fault at `fault_rip`, runs `access`, and disarms it.
fn run_probe(fault_rip: u64, access: impl FnOnce() -> u64) -> Probe {
    PROBE_RIP.store(fault_rip, Ordering::SeqCst);
    let result = access();
    PROBE_RIP.store(0, Ordering::SeqCst);
    if result == COMPLETED {
        Probe::Completed
    } else {
        Probe::Faulted {
            error: result,
            address: PROBE_ADDRESS.load(Ordering::SeqCst),
        }
    }
}

/// Reads the byte at `addr`.
///
/// # Safety
///
/// Reading `addr` must have no side effect if it succeeds: not MMIO.
pub(crate) unsafe fn probe_read(addr: u64) -> Probe {
    // SAFETY: the caller's contract; `read_byte` faults at its first instruction or reads
    // one byte and returns.
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
pub(crate) unsafe fn probe_write(addr: u64, value: u8) -> Probe {
    // SAFETY: the caller's contract; `write_byte` faults at its first instruction or
    // stores `value` (unchanged memory) and returns.
    run_probe((write_byte as *const ()).addr() as u64, || unsafe {
        write_byte(addr, value)
    })
}

/// Jumps into [`PROBE_CODE`], writable data, which NX must refuse to execute. Returns the
/// probed address too.
pub(crate) fn probe_execute_data() -> (u64, Probe) {
    let target = (&raw const PROBE_CODE).addr() as u64;
    // SAFETY: `PROBE_CODE` holds `mov rax, -1; ret`, so the jump either faults or runs it.
    let probe = unsafe { probe_execute(target) };
    (target, probe)
}

/// The bytes of `mov rax, -1; ret`, for a probe target that must not be executable.
pub(crate) const PROBE_RETURN: [u8; 8] = [0x48, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF, 0xC3];

/// Jumps to `target` and reports whether the fetch faulted.
///
/// # Safety
///
/// If the fetch is allowed, the code at `target` runs: it must be [`PROBE_RETURN`].
pub(crate) unsafe fn probe_execute(target: u64) -> Probe {
    // SAFETY: the fetch at `target` either faults (and resumes at `probe_recover`, which
    // returns to this caller) or runs `PROBE_RETURN` (the caller's contract), which only
    // sets RAX and returns.
    run_probe(target, || unsafe { jump(target) })
}

/// CR4.SMEP: supervisor instruction fetches from user pages fault.
const CR4_SMEP: u64 = 1 << 20;
/// CR4.SMAP: supervisor data accesses to user pages fault (unless RFLAGS.AC is set).
const CR4_SMAP: u64 = 1 << 21;

/// Which of SMEP and SMAP this processor supports (CPUID 7.0 EBX bits 7 and 20).
pub(crate) fn smep_smap_supported() -> (bool, bool) {
    if cpuid(0)[0] < 7 {
        return (false, false);
    }
    let ebx = cpuid(7)[1];
    (ebx & (1 << 7) != 0, ebx & (1 << 20) != 0)
}

/// Enables SMEP and SMAP on this processor, where supported; returns which were enabled.
///
/// # Safety
///
/// CPL 0. The kernel must not rely on executing or accessing user pages from supervisor
/// mode (it has none; a future user-copy path must set RFLAGS.AC around its accesses).
pub(crate) unsafe fn enable_smep_smap() -> (bool, bool) {
    let (smep, smap) = smep_smap_supported();
    let bits = if smep { CR4_SMEP } else { 0 } | if smap { CR4_SMAP } else { 0 };
    // SAFETY: the caller's contract; setting supported CR4 bits only adds restrictions on
    // supervisor accesses to user pages.
    unsafe {
        asm!(
            "mov {tmp}, cr4",
            "or {tmp}, {bits}",
            "mov cr4, {tmp}",
            bits = in(reg) bits,
            tmp = out(reg) _,
            options(nomem, nostack, preserves_flags),
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

/// Saves the general-purpose registers, calls [`dispatch`] with a pointer to the frame,
/// switches to the frame `dispatch` returns (the same one, or another thread's saved frame
/// with the same layout), restores its registers, drops the vector and error code, and
/// returns from the interrupt into it.
///
/// On entry the stack holds, from the top: vector, error code, then the processor's frame.
/// The processor aligned RSP to 16 bytes before its 40-byte frame; with the 16 bytes of
/// error code and vector and 120 bytes of registers, RSP is 16-byte aligned at the `call`.
#[unsafe(naked)]
extern "sysv64" fn exception_common() {
    naked_asm!(
        "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
        "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
        "mov rdi, rsp",
        "cld",
        "call {dispatch}",
        // `dispatch` returns the saved frame to resume: this one, or another thread's.
        "mov rsp, rax",
        "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
        "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
        "add rsp, 16",
        "iretq",
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

/// Entry stubs of the scheduler's software interrupts: no error code, so push 0.
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
