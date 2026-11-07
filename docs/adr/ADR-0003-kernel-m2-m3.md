# ADR-0003: Framekernel memory, interrupts, SMP start-up, first scheduler and first user mode (M2, M3, M4 groundwork)

| | |
|---|---|
| Status | **Proposed** (2026-11-07). Implemented and boot-tested in QEMU; awaiting the owner. |
| Scope | `framekernel/` from M2 to the first M3 steps |
| Requires | CLAUDE.md §18 (ADRs for memory allocator, scheduler, locking, kernel architecture) |
| Related | ADR-0002 (boot; boot info 0.2), ONBF §6.1 (KRN segments) |

## Context

M2 needs the kernel to own its CPU tables, memory and interrupts; M3 needs real multicore execution and preemption (CLAUDE.md §24–§28, final rules 5–9). Each choice below is the smallest correct design that the boot test can prove, with a negative control for each proof. None of them is an ABI: all can change without a format or compatibility decision.

## Decisions

### K1. The library is safe code; hardware access is capability-typed in the binary
`framekernel` (lib) is `forbid(unsafe_code)` and host-tested: descriptors, frames, paging, kernel_space, heap, apic, acpi, runqueue. The binary holds every `unsafe` operation, one per block, each with a SAFETY argument. Hardware is reached through types that encode what was proven (`X2Apic` exists only after the mode switch; its safe methods cannot reach the ICR; `send_ipi` is a separate `unsafe fn`).
*Alternatives:* a single crate with `unsafe` throughout (less testable); a separate HAL crate (premature).

### K2. Address space: identity mapping with exact permissions, until a higher-half ADR

*Superseded on 2026-11-07 by ADR-0004 (accepted and implemented): the kernel now runs in the higher half with a direct map; the permission rules below carry over unchanged.*

The kernel keeps virtual = physical, but in its own tables: each kernel segment with the access the signed ONBF segment table declares (boot info 0.2), a read/write never-executable direct map of the RAM the kernel may touch, page 0 and everything else unmapped, no alias of kernel code. EFER.NXE, CR0.WP, and on every CPU CR4.SMEP and CR4.SMAP. Guard pages below every thread and AP stack.
*Alternatives:* higher-half kernel with KASLR now (needs loader-built tables and a virtual layout decision: deferred to its own ADR, ADR-0002 B3); 1 GiB direct-map pages (fewer tables, but coarser holes).
*Consequence:* user address spaces (M4) must take the lower half away from the identity map; that migration is the higher-half ADR.

### K3. Physical memory: a bitmap frame allocator; strict frees
One bit per 4 KiB frame over `Usable` regions; next-fit single frames and first-fit runs. A free of a frame the allocator never owned (frame 0, reserved, non-`Usable`) is refused.
*Alternatives:* buddy allocator (better for large runs; to evaluate when runs matter), per-CPU frame caches (for M3 scalability, later).

### K4. Kernel heap: bitmap slabs plus frame runs
Power-of-two blocks of 16–2048 bytes in one-frame slabs, free blocks tracked beside the slab (so the bookkeeping never touches heap memory), larger allocations as frame runs; every free validated. One global spin lock.
*Alternatives:* intrusive free lists (faster, but writes into freed memory and needs `unsafe` in the library); per-CPU magazines (later, for scalability).

### K5. Interrupts: x2APIC only, PIT-calibrated timer
2026-era Intel and AMD platforms implement x2APIC, so xAPIC/MMIO is not supported. The 8259s are masked. The APIC timer and TSC are calibrated against a PIT channel-2 one-shot. The scheduler tick is 100 Hz.
*Alternatives:* TSC-deadline mode (better; needs invariant-TSC and frequency discovery), HPET or ACPI PM timer calibration, 1 kHz ticks (QEMU TCG coalesced them; to reconsider on hardware).

### K6. SMP start-up: MADT-driven INIT-SIPI-SIPI with a W^X trampoline
Only processors the MADT marks enabled are started, one at a time. The trampoline runs from a reserved frame remapped read/execute only while APs start; its mutable data is on a separate read/write frame. Each AP gets heap-allocated GDT, TSS (#DF stack) and kernel stack, shares the IDT, and enables x2APIC, SMEP and SMAP.
*Alternatives:* broadcast INIT-SIPI (starts disabled CPUs: rejected); parallel start-up (faster boot; later, with per-AP trampoline data).

### K7. First scheduler: per-CPU round-robin, context = interrupt frame
A thread's saved context is its interrupt frame on its own stack; the timer handler returns the frame to resume. One run queue per CPU (indexed by x2APIC ID for now) behind its own lock, taken with interrupts disabled outside the handler. Threads are never migrated.
*Alternatives:* a separate `switch_to` with callee-saved registers only (cheaper for voluntary switches; to add with blocking); EEVDF-style fair scheduling (to evaluate once this baseline is proven, §26).

### K8. First user mode: one DPL-3 gate, RSP0 per switch, faults end the thread
Ring-3 code reaches the kernel only through `int 0x80` (DPL 3); every other gate is DPL 0. The kernel enters user mode with every general-purpose register zeroed and DPL-3 data selectors loaded (so the CPL change nulls no segment register and the kernel's GS base survives; user mode cannot read it). The scheduler points TSS.RSP0 at the resumed thread's kernel stack on every switch. An exception in user mode ends the thread, not the kernel.
`syscall` enters through the same path (built frame, IRETQ return); NMI and #MC run on IST2 and IST3 so they never use a user stack.
*Alternatives:* SYSRET for the return and `swapgs` (faster, the M4 target; SYSRET needs a canonical-RCX check); per-CPU RSP0 written only for user threads (cheaper, but easy to get wrong).

## Evidence

All in QEMU 11.1.3 (TCG, `-cpu max`, OVMF), `cargo xtask boot-test`, with `ORIGIN_QEMU_SMP` = 2, 4 and 8. Every proof has a negative control that makes the boot test fail. See `docs/engineering/SOURCE_PROVENANCE.md` (framekernel M2 and M3) for the numbers.

## Consequences and open points

- Physical hardware qualification is still required (§74); the timer bound in particular must be tightened there.
- Since this ADR was written (same day): the boot processor's stack has a guard page (the loader allocates one extra `KernelStack` page), `LoaderReclaimable` memory is reclaimed once every enabled processor is online, and threads can yield, sleep and exit (software interrupts 0x81–0x83 on the same switch path). Exited threads' stacks are not freed yet.
- Loom models for the spin lock and Kani bounds for paging, frames and heap are not written yet (§73).
- Per-CPU data: since this ADR was written, a GS-based CPU-local area replaced reading the x2APIC ID (about 18 % fewer cycles per context switch in QEMU TCG).
