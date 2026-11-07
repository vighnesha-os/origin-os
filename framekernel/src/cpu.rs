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

static mut GDT: Gdt = Gdt([0; GDT_ENTRIES]);
static mut TSS_TABLE: TssImage = TssImage([0; 26]);
static mut IDT: Idt = Idt([[0; 2]; 256]);
static mut DOUBLE_FAULT_STACK: Stack = Stack([0; DOUBLE_FAULT_STACK_SIZE]);

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
    let stack_top = (&raw mut DOUBLE_FAULT_STACK).addr() + DOUBLE_FAULT_STACK_SIZE;
    // SAFETY: the static is used only here and by the processor through the TSS; this is
    // the single, single-threaded initialisation (the caller's contract).
    unsafe { (&raw mut TSS_TABLE).write(TssImage(d::tss_image(stack_top as u64))) };
    let tss = d::tss_descriptor((&raw const TSS_TABLE).addr() as u64);
    let gdt = Gdt([
        0,
        d::kernel_code_segment(),
        d::kernel_data_segment(),
        tss[0],
        tss[1],
    ]);
    // SAFETY: as above; the GDT is written before `lgdt` points the processor at it.
    unsafe { (&raw mut GDT).write(gdt) };

    let mut idt = Idt([[0; 2]; 256]);
    for (vector, (gate, stub)) in idt.0.iter_mut().zip(STUBS.iter()).enumerate() {
        let ist = if vector == 8 { DOUBLE_FAULT_IST } else { 0 };
        *gate = d::interrupt_gate(*stub as usize as u64, KERNEL_CODE, ist);
    }
    let interrupts: [(u8, extern "sysv64" fn()); 2] =
        [(TIMER_VECTOR, timer_stub), (SPURIOUS_VECTOR, spurious_stub)];
    for (vector, stub) in interrupts {
        if let Some(gate) = idt.0.get_mut(usize::from(vector)) {
            *gate = d::interrupt_gate(stub as usize as u64, KERNEL_CODE, 0);
        }
    }
    // SAFETY: as above; written before `lidt`.
    unsafe { (&raw mut IDT).write(idt) };

    let gdt_ptr = TablePointer {
        limit: (GDT_ENTRIES * 8 - 1) as u16,
        base: (&raw const GDT).addr() as u64,
    };
    let idt_ptr = TablePointer {
        limit: (256 * 16 - 1) as u16,
        base: (&raw const IDT).addr() as u64,
    };
    // SAFETY: `gdt_ptr` describes the fully written GDT above. After `lgdt`, CS is reloaded
    // with a far return to the next instruction and the data segments with the kernel data
    // selector, so no register keeps a selector into the firmware's GDT. `ltr` loads the TSS
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
    // SAFETY: `idt_ptr` describes the fully written IDT; every present gate points at a stub
    // below in the kernel code segment just loaded.
    unsafe { asm!("lidt [{idt}]", idt = in(reg) &raw const idt_ptr, options(nostack)) };
    let _ = TSS_SIZE; // the limit is encoded by `tss_descriptor`
}

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
extern "sysv64" fn dispatch(frame: &mut ExceptionFrame) {
    // Device interrupts: the timer counts and acknowledges; a spurious interrupt needs no
    // EOI (SDM Vol. 3A, spurious interrupt).
    if frame.vector == u64::from(TIMER_VECTOR) {
        TICKS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: x2APIC mode is enabled before the timer is unmasked (`timer::init`), and
        // writing 0 to the EOI register only acknowledges the interrupt in service.
        unsafe { wrmsr(X2APIC_EOI, 0) };
        return;
    }
    if frame.vector == u64::from(SPURIOUS_VECTOR) {
        return;
    }
    // A page fault at the exact instruction a probe armed: record it and resume at
    // `probe_recover`, which returns the error code to the probe's caller.
    if frame.vector == 14 {
        let armed = PROBE_RIP.load(Ordering::SeqCst);
        if armed != 0 && frame.rip == armed {
            PROBE_ADDRESS.store(read_cr2(), Ordering::SeqCst);
            frame.rax = frame.error_code;
            frame.rip = (probe_recover as *const ()).addr() as u64;
            return;
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
        return;
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
static mut PROBE_CODE: [u8; 8] = [0x48, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF, 0xC3];

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
    // SAFETY: the fetch at `target` either faults (and resumes at `probe_recover`, which
    // returns to this caller) or executes `PROBE_CODE`, which only sets RAX and returns.
    let probe = run_probe(target, || unsafe { jump(target) });
    (target, probe)
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
/// restores them, drops the vector and error code, and returns from the exception.
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
