//! The boot flow (M1 onwards): everything the kernel does between the boot loader's hand-over
//! and the end of its proofs, in order.
//!
//! The kernel programs COM1 (its UART until the ACPI tables name a better one), checks that its
//! own relocations were applied, validates the boot info, attaches the firmware's framebuffer
//! if the loader passed one, and prints a summary. Then come the M2 steps, through the
//! framework's [`Early`]: its own GDT, TSS and IDT (proven with a breakpoint), the frame
//! allocator, and its own page tables with a W^X kernel image (proven with deliberate faults);
//! then the heap, the application processors, the drivers, and the proofs that each works.

use core::fmt::Write;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::bootproof::{Proof, Tally};
use origin_boot_info::{BootInfo, SEGMENT_EXECUTE, SEGMENT_WRITE};
use origin_frame::early::{Early, Entry};

use crate::console::Console;
use crate::cpu::halt;
use crate::report;
use crate::serial::Uart16550;
use crate::{
    console, cpu, i8042, iommu, irq, memory, pcie, sched, smp, storage, syscall, time, timer,
    virtionet,
};

/// I/O port base of COM1 on PC-compatible platforms.
pub(crate) const COM1_BASE: u16 = 0x3F8;

/// Set by the first panic. A panic while reporting a panic only halts.
static PANICKING: AtomicBool = AtomicBool::new(false);

/// The kernel's Rust entry: `_start` jumps here with RBP zero, the stack aligned, and the
/// loader's boot-info pointer as the [`Entry`] in RDI.
pub extern "sysv64" fn kernel_main(entry: Entry) -> ! {
    // The monotonic clock's zero (CLAUDE.md §68).
    time::mark_entry();
    // COM1 is the UART of last resort: nothing better is known until the ACPI tables can
    // be read, which `console::adopt_firmware_uart` does.
    let mut uart = Uart16550::new(console::KernelUart::com1());
    let firmware_line = uart.line_settings();
    uart.init();

    if !entry.relocations_applied() {
        // `core::fmt` would call through unrelocated vtables. `write_str` on the concrete
        // type is a direct call, and the string is addressed RIP-relative.
        let _ = uart.write_str(report::RELOCATIONS_NOT_APPLIED);
        halt();
    }
    // From here on every processor prints through the console, a line at a time.
    console::init(uart, firmware_line);
    let mut com1 = console::Console::boot_flow();
    // What each boot proof found, and what that means for this boot (ADR-0013).
    let mut proofs = Tally::new();

    let mut early = match entry.read_boot_info() {
        Ok(early) => early,
        Err(err) => {
            let _ = report::write_invalid(&mut com1, err);
            halt();
        }
    };
    let data = early.data();
    let (info, regions, segments, memory) = (
        *data.info(),
        data.regions(),
        data.segments(),
        data.summary(),
    );
    // The firmware's framebuffer, drawn on from here on (ADR-0010). The earlier lines of a
    // boot this far can only have been a report of an invalid boot info, on the UART.
    console::attach_framebuffer(&mut com1, &info, regions);
    let _ = report::write_report(&mut com1, &info, &memory);
    for (i, s) in segments.iter().enumerate() {
        let access = match (
            s.access & SEGMENT_WRITE != 0,
            s.access & SEGMENT_EXECUTE != 0,
        ) {
            (false, false) => "R",
            (true, false) => "RW",
            (false, true) => "RX",
            (true, true) => "RWX",
        };
        let _ = writeln!(
            com1,
            "origin-kernel: kernel segment {i}: {access} at {:#x}, {} pages",
            info.kernel_virtual_base.wrapping_add(s.rva),
            s.pages
        );
    }

    // Out of turn the boot steps do nothing, whoever calls them (`Early`): proven here, before
    // any of them has run, and below for a second run of each.
    let out_of_turn = !early.start_heap(&mut com1)
        && !early.prove_user_isolation(&mut com1)
        && early.build_kernel_tables(&mut com1).is_none()
        && timer::init(&mut com1, info.acpi_rsdp).is_none();

    // The trap entry (in `cpu`) knows the layers above only through these upcalls.
    origin_frame::trap::install(origin_frame::trap::Hooks {
        tick: sched::on_tick,
        reschedule: sched::on_reschedule,
        yield_now: sched::on_yield,
        sleep: sched::on_sleep,
        block: sched::on_block,
        exit: sched::on_exit,
        syscall: syscall::run,
        user_fault: syscall::user_fault,
        report_exception: console::report_exception,
    });
    // M2, first step: the kernel's own GDT, TSS and IDT, proven with a breakpoint whose
    // handler reports it and resumes here.
    early.load_cpu_tables();
    let _ = report::write_cpu_tables(&mut com1);
    cpu::breakpoint();

    // CPU features and the x86-64-v3 baseline (CLAUDE.md §14, §24), then SSE/AVX enabled on
    // the boot processor.
    let cpuid = cpu::gather_cpuid();
    let features = crate::features::Features::decode(&cpuid);
    let xcr0 = early.enable_simd();
    let _ = writeln!(
        com1,
        "origin-kernel: CPU: x86-64 level {} (baseline v3 {}); SSE {}, AVX2 {}, FMA {}, BMI2 {}, RDSEED {}, PCID {}, INVPCID {}, FSGSBASE {}; XSAVE area {} bytes, XCR0 {:#x}",
        features.arch_level().unwrap_or(0),
        features.meets_baseline(),
        features.sse2,
        features.avx2,
        features.fma,
        features.bmi2,
        features.rdseed,
        features.pcid,
        features.invpcid,
        features.fsgsbase,
        cpu::fpu_area_size(),
        xcr0
    );
    // CLAUDE.md §14: Origin OS requires x86-64-v3. Below it — or if AVX state could not be
    // enabled (XCR0 must hold x87, SSE and AVX state) — refuse to go on, naming what is
    // missing, rather than fail later in a way that hides why.
    let avx_state = xcr0 & 0b111 == 0b111;
    if !features.meets_baseline() || !avx_state {
        let _ = write!(
            com1,
            "origin-kernel: this CPU is below the x86-64-v3 baseline Origin OS requires (CLAUDE.md §14); missing:"
        );
        for name in features.missing_baseline() {
            let _ = write!(com1, " {name}");
        }
        if !avx_state {
            let _ = write!(com1, " AVX state in XCR0 ({xcr0:#x})");
        }
        let _ = writeln!(com1, ": CPU baseline failed");
        halt();
    }
    proofs.record(Proof::Backtrace, prove_backtrace(&mut com1, &info));

    // M2, second step: the physical frame allocator over the boot-info regions, with a
    // pool of frames for page tables beside its bitmap.
    if !early.init_memory(&mut com1) {
        halt();
    }

    // M2, third step: the kernel's own page tables, proven with deliberate faults; then
    // the frame allocator is proven again through the new direct map.
    let mut heap_ok = false;
    let paging = init_paging(&mut com1, &mut early);
    // The kernel's own tables are loaded, so firmware tables can be mapped: the console's
    // UART is the one the firmware names, if it names one (ADR-0010).
    console::adopt_firmware_uart(&mut com1, info.acpi_rsdp, regions);
    // After the UART is settled, so the refusal reaches the console the firmware names.
    record_or_stop(&mut com1, &mut proofs, Proof::PageTables, paging);
    // Threat T4: stop every device the firmware left bus-mastering as soon as the kernel can
    // reach configuration space, before it claims any more memory for itself.
    let mut quiesced = None;
    if paging {
        // M2: the kernel heap, which takes over the frame allocator; from here on the
        // `alloc` crate works in the kernel.
        early.start_heap(&mut com1);
        record_or_stop(
            &mut com1,
            &mut proofs,
            Proof::FrameAllocator,
            early.frames_proven(),
        );
        quiesced = pcie::quiesce(
            &mut com1,
            info.acpi_rsdp,
            regions,
            &iommu::own_functions(info.acpi_rsdp),
        );
        // Every driver is refused if this failed (`DeviceDma`): only the verdict is left to do.
        proofs.record(
            Proof::BusMasterQuiesce,
            quiesced.is_some() && pcie::quiesced(),
        );
        heap_ok = prove_heap(&mut com1);
        record_or_stop(&mut com1, &mut proofs, Proof::Heap, heap_ok);
        if heap_ok && !early.prove_user_isolation(&mut com1) {
            // Ring 3 comes next: a kernel that cannot keep itself from user pages must not
            // run it, any more than a processor below the baseline (and `prove_user_isolation`
            // has said why).
            let _ = writeln!(
                com1,
                "origin-kernel: supervisor protection from user pages is not in force: user isolation failed"
            );
            halt();
        }
    }

    // M2, fourth step: interrupts. The local APIC in x2APIC mode, its timer calibrated
    // against the PIT and proven by taking periodic interrupts.
    let tsc_hz = timer::init(&mut com1, info.acpi_rsdp);
    record_or_stop(&mut com1, &mut proofs, Proof::ApicTimer, tsc_hz.is_some());
    if let Some(hz) = tsc_hz {
        // The timer runs and every vector that can arrive has a gate: from here on, the
        // boot flow takes interrupts between the sections of the lines it prints.
        console::allow_interrupt_windows();
        if let Some(q) = quiesced.as_mut() {
            proofs.record(Proof::BusMastersSettled, pcie::settle(&mut com1, q, hz));
        }
        let nmi = prove_nmi_ist(&mut com1, hz);
        proofs.record(Proof::NmiDelivery, nmi.delivered);
        if nmi.delivered {
            record_or_stop(&mut com1, &mut proofs, Proof::NmiStack, nmi.on_own_stack);
        }
        // The clocks: the monotonic clock's TSC rate, proven against the RTC, and the wall
        // clock from the RTC (CLAUDE.md §68).
        proofs.record(Proof::Clocks, time::init(&mut com1, hz, info.acpi_rsdp));
        // I/O APIC interrupt routing, proven by routing the PIT's IRQ0 (CLAUDE.md §24).
        let bsp =
            timer::X2Apic::this_cpu().map_or(u32::MAX, |l| l.read(timer::ApicRead::Id) as u32);
        proofs.record(
            Proof::IoApic,
            irq::prove(&mut com1, info.acpi_rsdp, bsp, hz),
        );
    }

    // M3, first step: start the application processors the ACPI MADT lists. It needs the
    // heap, the kernel's own tables and the calibrated TSC.
    if let Some(tsc_hz) = tsc_hz
        && heap_ok
    {
        // DMA and interrupt remapping (Intel VT-d or AMD-Vi) before the application
        // processors start, the loader's memory is reclaimed or any driver binds: translation
        // on with no device attached, so every DMA faults until a driver attaches its device,
        // including a device that ignored its cleared Bus Master Enable (CLAUDE.md §32, §58;
        // ADR-0007). It needs the clock (`time::init`) for its timeouts, and comes after the
        // compatibility-format I/O APIC proof above, which interrupt remapping would block.
        // It runs whether or not the bus-master quiesce succeeded: confining a device that kept
        // mastering the bus is exactly what a failed quiesce leaves to it.
        proofs.record(
            Proof::Iommu,
            iommu::init(&mut com1, info.acpi_rsdp, pcie::read_config32),
        );
        if iommu::translating() {
            let _ = writeln!(
                com1,
                "origin-kernel: IOMMU: translating {} ms after kernel entry",
                time::monotonic_nanos() / 1_000_000
            );
        }
        let online = smp::start(&mut com1, sched::run_ap);
        proofs.record(Proof::Processors, online.complete());
        // What each processor put in its own control registers, which ring 3 relies on.
        let held = cpu::cpu_states_holding(online.cpus()) == online.cpus().len();
        record_or_stop(&mut com1, &mut proofs, Proof::CpuState, held);
        // ADR-0017: what each processor reported and set as it started, and the two forms
        // of the lookup every index and address from ring 3 goes through.
        let controls = prove_speculation(&mut com1, online.cpus());
        proofs.record(Proof::SpeculationControls, controls);
        let clamps = prove_nospec(&mut com1);
        record_or_stop(&mut com1, &mut proofs, Proof::Nospec, clamps);
        // M2 completion: once every enabled processor has left firmware code, the loader's
        // and the firmware's boot-time memory belongs to the kernel.
        let reclaimed = early.reclaim_loader_memory(&online);
        if let Some((added, free)) = reclaimed {
            let _ = writeln!(
                com1,
                "origin-kernel: reclaimed {} MiB of loader and boot-services memory; {} MiB free",
                (added * memory::FRAME) >> 20,
                (free * memory::FRAME) >> 20
            );
        }
        // Every boot step, and the two that bring up hardware for good, run once: a repeat
        // changes nothing (`smp::start` would otherwise reset running processors).
        let again = smp::start(&mut com1, sched::run_ap);
        let once = again.cpus().len() == 1
            && !again.complete()
            && early.reclaim_loader_memory(&online).is_none()
            && !early.init_memory(&mut com1)
            && early.build_kernel_tables(&mut com1).is_none()
            && !early.start_heap(&mut com1)
            && !early.prove_user_isolation(&mut com1)
            && early.enable_simd() == xcr0
            && !iommu::init(&mut com1, info.acpi_rsdp, pcie::read_config32);
        let _ = writeln!(
            com1,
            "origin-kernel: boot steps: out of turn refused {out_of_turn}; a second run refused {once}{}",
            if out_of_turn && once {
                ""
            } else {
                ": boot steps failed"
            }
        );
        record_or_stop(
            &mut com1,
            &mut proofs,
            Proof::BootSteps,
            out_of_turn && once,
        );
        // CPU topology from CPUID, each processor's on itself, checked against the MADT
        // (CLAUDE.md §24, §27).
        let apic_ids = online.apic_ids();
        let topology = prove_topology(&mut com1, info.acpi_rsdp, &online);
        proofs.record(Proof::Topology, topology);
        // NUMA affinities from the ACPI SRAT (CLAUDE.md §24, §28).
        let numa = prove_numa(&mut com1, info.acpi_rsdp, &apic_ids);
        proofs.record(Proof::Numa, numa);
        // Inter-node distances from the ACPI SLIT (CLAUDE.md §27, §28).
        proofs.record(Proof::NumaDistances, prove_slit(&mut com1, info.acpi_rsdp));
        // Hardware entropy from RDSEED/RDRAND (CLAUDE.md §59).
        proofs.record(Proof::Entropy, prove_entropy(&mut com1, &features));
        // M3, second step: preemptive kernel threads on every online processor. Its proofs
        // run to the end and are judged together (ADR-0013 R4): the proofs after a failure
        // use only the kernel's fixed programs, and the operator sees every result.
        sched::prove(&mut com1, online.cpus(), tsc_hz, &mut proofs);
        // ADR-0017 Z3, after the proofs above have run several processes in ring 3.
        proofs.record(Proof::Ibpb, prove_ibpb(&mut com1, online.cpus()));
        stop_if_refused(&mut com1, proofs);
        // M5 groundwork: PCI Express enumeration through the ACPI MCFG.
        if let Some(q) = quiesced.as_mut()
            && let Some(reserved) = pcie::reserved_ranges(&mut com1, q, info.acpi_rsdp)
            && pcie::init(&mut com1, q, online.cpus(), regions, &reserved)
        {
            proofs.record(Proof::Pcie, true);
            // Anything that turned bus mastering back on is stopped again before any driver
            // attaches.
            proofs.record(Proof::BusMastersQuiet, pcie::check_quiet(&mut com1, q));
            // M5: the NVMe driver and block I/O, polled and then interrupt-driven.
            proofs.record(Proof::Nvme, storage::init(&mut com1, online.cpus(), tsc_hz));
            // M10: bring up the virtio-net NIC; an interrupt-driven receive thread takes the
            // OS onto the network (CLAUDE.md §31, §54). The one proof not in the verdict yet
            // (ADR-0013): `virtionet` returns nothing, and it prints each failure itself.
            virtionet::prove(&mut com1, online.cpus(), tsc_hz);
            // The IOMMU's own proofs, through the bound NVMe controller: translated DMA, a
            // blocked and recorded DMA, remapped and blocked interrupts.
            let devices = storage::prove_iommu(&mut com1, online.cpus(), tsc_hz);
            proofs.record(Proof::IommuDevices, devices);
            // Legacy interrupts under interrupt remapping: the PIT's IRQ0 through the I/O
            // APIC in remappable format and an IRTE that checks the I/O APIC's source ID,
            // with the IRTE made not present and given a wrong source ID (CLAUDE.md §24,
            // §32; ADR-0007 I8).
            let remapped = irq::prove_remapped(&mut com1, online.cpus(), tsc_hz);
            proofs.record(Proof::IoApicRemapped, remapped);
            // Bus mastering is on exactly for what the drivers attached to a domain.
            let mastering = pcie::prove_bus_mastering(&mut com1, q);
            proofs.record(Proof::BusMastering, mastering);
        } else {
            proofs.record(Proof::Pcie, false);
        }
        sched::stop_boot_processor();
    }

    // M8/M9 groundwork: the in-memory VFS (paths, directories, files).
    proofs.record(Proof::Vfs, prove_vfs(&mut com1));
    // §21 groundwork: extract an embedded initramfs (a ustar image) into a VFS.
    proofs.record(Proof::Initramfs, prove_initramfs(&mut com1));
    // M8 groundwork: the software rasterizer draws a fixed scene whose CRC must match the
    // host-proven one, bit for bit.
    proofs.record(Proof::Raster, prove_raster(&mut com1));
    // M10 groundwork: the i8042 PS/2 controller bring-up (CLAUDE.md §56).
    proofs.record(Proof::I8042, i8042::init(&mut com1, info.acpi_rsdp));
    // Networking groundwork: build and parse an ICMP echo through a loopback (CLAUDE.md §54).
    proofs.record(Proof::Net, prove_net(&mut com1));
    // TCP: a full connection over an in-memory loopback (CLAUDE.md §54).
    proofs.record(Proof::Tcp, prove_tcp(&mut com1));
    // The monotonic timer queue, driven by the real clock (CLAUDE.md §68).
    proofs.record(Proof::TimerQueue, prove_timers(&mut com1));
    // Integration: a read-only /sys tree in the VFS carrying live discovered facts.
    proofs.record(Proof::Sysfs, prove_sysfs(&mut com1));
    // The console's sending and drawing in sections, proven last so that what it prints is
    // still on the screen when the boot test compares the screen with the serial log.
    let uart = console::prove_uart_sections(&mut com1);
    proofs.record(Proof::ConsoleUartSections, uart);
    let screen = console::prove_sections(&mut com1);
    proofs.record(Proof::ConsoleSections, screen);

    // The operator's summary: everything above scrolls past on a real machine's screen.
    let _ = proofs.write_verdict(&mut com1);
    let _ = report::write_halt(&mut com1);
    halt()
}

/// Records one proof's result, and stops the boot if that result says to (ADR-0013).
fn record_or_stop(com1: &mut Console, proofs: &mut Tally, proof: Proof, passed: bool) {
    proofs.record(proof, passed);
    stop_if_refused(com1, *proofs);
}

/// Halts if a recorded failure means the kernel must stop (ADR-0013 R1), having said which.
fn stop_if_refused(com1: &mut Console, proofs: Tally) {
    if proofs.must_stop() {
        let _ = proofs.write_refusal(com1);
        halt();
    }
}

/// ADR-0017 Z1: prints what the processors reported and set as they started, and says
/// whether each one's registers read back as written and whether any lacks something the ADR
/// needs. A machine that lacks a control still boots (decision item 131): this is a report.
fn prove_speculation(com1: &mut Console, cpus: &[cpu::CpuIndex]) -> bool {
    use origin_frame::speculation;
    let mut alike = true;
    let mut first = None;
    let mut sound = !cpus.is_empty();
    for c in cpus {
        let Some((plan, held)) = speculation::recorded(c.get()) else {
            let _ = writeln!(
                com1,
                "origin-kernel: speculation: processor {} recorded nothing",
                c.get()
            );
            sound = false;
            continue;
        };
        if !held {
            let _ = writeln!(
                com1,
                "origin-kernel: speculation: processor {}: a control did not read back as written",
                c.get()
            );
        }
        sound &= held && plan.complete();
        alike &= *first.get_or_insert(plan) == plan;
    }
    match first {
        Some(plan) if alike => {
            let _ = writeln!(
                com1,
                "origin-kernel: speculation: {} processors alike: {plan}",
                cpus.len()
            );
        }
        _ => {
            for c in cpus {
                if let Some((plan, _)) = speculation::recorded(c.get()) {
                    let _ = writeln!(
                        com1,
                        "origin-kernel: speculation: processor {}: {plan}",
                        c.get()
                    );
                }
            }
        }
    }
    sound
}

/// ADR-0017 Z9: runs both forms of the lookup, as the instructions the kernel uses, over the
/// ends of the range, and prints what each costs.
///
/// It shows what the sequences compute. What a processor does on a wrongly guessed path it
/// cannot show: that rests on Intel's and AMD's documents.
fn prove_nospec(com1: &mut Console) -> bool {
    use origin_frame::nospec::{self, Form};
    use origin_frame::usercopy::{USER_END, USER_START};
    const FORMS: [Form; 2] = [Form::Mask, Form::Fence];
    const LENGTHS: [usize; 6] = [0, 1, 2, 255, 0xFFFF, usize::MAX];
    const INDICES: [usize; 9] = [
        0,
        1,
        2,
        254,
        255,
        0xFFFE,
        0xFFFF,
        usize::MAX - 1,
        usize::MAX,
    ];
    let mut sound = true;
    for form in FORMS {
        for len in LENGTHS {
            for index in INDICES {
                let got = nospec::index_in(form, index, len);
                sound &= got == (index < len).then_some(index);
            }
        }
        for address in [USER_START, USER_END - 1] {
            sound &= nospec::user_address_in(form, address) == address;
        }
    }
    // The mask puts 0 in place of anything at or above the limit, and of nothing below it.
    for limit in [0, 1, 0x100, USER_END, u64::MAX] {
        for value in [0, 1, 0xFF, 0x100, USER_END - 1, USER_END, 1 << 63, u64::MAX] {
            let want = if value < limit { value } else { 0 };
            sound &= cpu::below_or_zero(value, limit) == want;
            sound &= cpu::after_fence(value) == value;
        }
    }
    for outside in [USER_END, 0xFFFF_8000_0000_0000, u64::MAX] {
        sound &= nospec::user_address_in(Form::Mask, outside) == 0;
    }
    // The form in use is the one the boot processor's maker and family call for.
    let reported = cpu::read_speculation();
    let chosen = nospec::choose(reported.maker(), reported.family());
    sound &= nospec::form() == chosen;

    const ROUNDS: u64 = 1 << 16;
    let cost = |form: Form| {
        let mut sum = 0usize;
        let start = time::monotonic_nanos();
        for round in 0..ROUNDS {
            let index = core::hint::black_box(round as usize & 0xFF);
            sum = sum.wrapping_add(nospec::index_in(form, index, 256).unwrap_or(0));
        }
        let taken = time::monotonic_nanos().saturating_sub(start);
        core::hint::black_box(sum);
        // Tenths of a nanosecond for one lookup, the loop around it included.
        taken.saturating_mul(10) / ROUNDS
    };
    let (masked, fenced) = (cost(Form::Mask), cost(Form::Fence));
    let _ = writeln!(
        com1,
        "origin-kernel: nospec: both forms keep an index and an address in range: {}; in use: {} ({:?}, family {}); a lookup costs {}.{} ns masked and {}.{} ns fenced, its loop included",
        if sound { "yes" } else { "NO" },
        match nospec::form() {
            Form::Mask => "mask",
            Form::Fence => "fence",
        },
        reported.maker(),
        reported.family(),
        masked / 10,
        masked % 10,
        fenced / 10,
        fenced % 10,
    );
    if !sound {
        let _ = writeln!(com1, "origin-kernel: nospec failed");
    }
    sound
}

/// ADR-0017 Z3: on every processor, one IBPB for each change of the process it runs in
/// ring 3 where the processor has the command, and none where it has not; and the scheduler
/// did note the processes the proofs before this one ran. Prints what one IBPB costs.
fn prove_ibpb(com1: &mut Console, cpus: &[cpu::CpuIndex]) -> bool {
    use origin_frame::speculation;
    let mut sound = true;
    let (mut changes, mut barriers, mut resumes) = (0u64, 0u64, 0u64);
    for c in cpus {
        let count = speculation::count(c.get());
        let offered = speculation::recorded(c.get()).is_some_and(|(plan, _)| plan.ibpb);
        sound &= count.barriers == if offered { count.changes } else { 0 };
        changes += u64::from(count.changes);
        barriers += u64::from(count.barriers);
        resumes += count.resumes;
    }
    // The proofs before this one ran more than one process in ring 3: each was a change on
    // the processor that took it up.
    sound &= changes >= 2;
    let _ = write!(
        com1,
        "origin-kernel: ibpb: {changes} changes of process on {} processors, {barriers} barriers, {resumes} resumes without one",
        cpus.len()
    );
    const SAMPLES: u64 = 16;
    let start = time::monotonic_nanos();
    let mut issued = 0;
    for _ in 0..SAMPLES {
        issued += u64::from(cpu::issue_ibpb());
    }
    let taken = time::monotonic_nanos().saturating_sub(start);
    let _ = if issued == SAMPLES {
        writeln!(com1, "; one IBPB costs {} ns", taken / SAMPLES)
    } else {
        writeln!(com1, "; this processor has no IBPB to measure")
    };
    if !sound {
        let _ = writeln!(com1, "origin-kernel: ibpb failed");
    }
    sound
}

/// Builds a read-only `/sys`-style tree in the VFS from facts several subsystems
/// discovered (free memory, the TSC rate, the PCIe device count, the NVMe disk size),
/// then reads values back and checks they parse to the discovered numbers. This shows the
/// VFS carrying a live system namespace built from real hardware state.
fn prove_sysfs(com1: &mut console::Console) -> bool {
    use crate::vfs::Vfs;
    // Gather the facts.
    let free_mib = memory::stats().map_or(0, |(_, free)| (free * memory::FRAME) >> 20);
    let tsc_mhz = time::tsc_hz() / 1_000_000;
    let pci = pcie::device_count() as u64;
    let disk_mib = storage::geometry().map_or(0, |(bs, blocks)| (blocks * u64::from(bs)) >> 20);

    // Build the tree.
    let mut fs = Vfs::new(1 << 16, 64);
    let mut n = 0u64;
    let mut write = |fs: &mut Vfs, path: &str, value: u64| {
        if fs
            .mkdir_all(path.rsplit_once('/').map_or("/", |(dir, _)| dir))
            .is_ok()
            && fs.create(path).is_ok()
        {
            let mut buf = [0u8; 20];
            let text = format_u64(value, &mut buf);
            if fs.write(path, 0, text).is_ok() {
                n += 1;
            }
        }
    };
    write(&mut fs, "/sys/mem/free_mib", free_mib);
    write(&mut fs, "/sys/cpu/tsc_mhz", tsc_mhz);
    write(&mut fs, "/sys/bus/pci/count", pci);
    write(&mut fs, "/sys/block/nvme0/size_mib", disk_mib);

    // Read one value back and check it parses to the discovered number.
    let mut buf = [0u8; 32];
    let read = fs.read("/sys/bus/pci/count", 0, &mut buf).unwrap_or(0);
    let parsed = core::str::from_utf8(&buf[..read])
        .ok()
        .and_then(|t| t.parse::<u64>().ok());
    let dirs = fs.list("/sys").map(|v| v.len()).unwrap_or(0);
    let ok = n == 4 && parsed == Some(pci) && dirs == 4;
    let _ = writeln!(
        com1,
        "origin-kernel: sysfs: built /sys with {n} facts ({dirs} subtrees): mem/free_mib={free_mib}, cpu/tsc_mhz={tsc_mhz}, bus/pci/count={pci}, block/nvme0/size_mib={disk_mib}; read back pci count {parsed:?}{}",
        if ok { "" } else { ": sysfs failed" }
    );
    ok
}

/// Writes `value` as decimal into `buf`, returning the written slice.
fn format_u64(value: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut v = value;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    &buf[i..]
}

/// Proves the monotonic timer queue against the real clock: three timers armed at
/// increasing deadlines (plus one cancelled) must fire in order, each close to its
/// deadline, and the cancelled one never (CLAUDE.md §68).
///
/// The queue looks at the time only when polled, and on a loaded host the polling loop's
/// virtual CPU can be away for milliseconds: a timer fired after such a stall is a late
/// look, not a late timer. So what the polls say about a firing is an interval, and a run
/// decides only if every firing is inside the limit (`Verdict::OnTime`) or one is outside it
/// (`Verdict::Late`: a poll that did not fire a timer already that overdue). A run a stall
/// left undecided is measured again, up to `TIMER_ATTEMPTS` runs; a timer that fires in the
/// wrong order, or one decisively late, fails at once.
fn prove_timers(com1: &mut console::Console) -> bool {
    use crate::timers::{Firing, TimerQueue, Verdict, judge};
    /// Runs the proof may make; each costs about 15 ms, and only a run a stall left undecided
    /// leads to another, so an undisturbed boot pays for one. Measured with two `yes` per
    /// logical core, a run was undecided in 55 % of cases, in streaks of up to 12 (six runs,
    /// the old budget, ran out in 4.4 % of cases); with four per core in 82 %, streaks up
    /// to 21. 24 runs never ran out in those measurements; this is that with room to spare.
    const TIMER_ATTEMPTS: usize = 64;
    /// How late a timer may be delivered, in nanoseconds.
    const LATENESS_LIMIT: u64 = 2_000_000;
    let step = 5_000_000u64; // 5 ms
    let ids = [10u64, 20, 30];
    let tsc_hz = time::tsc_hz();
    let mut order = alloc::vec::Vec::new();
    let mut worst_late = 0u64;
    let mut verdict = Verdict::Undecided;
    let mut attempts = 0;
    while attempts < TIMER_ATTEMPTS && verdict == Verdict::Undecided {
        attempts += 1;
        let mut q = TimerQueue::new();
        let base = time::monotonic_nanos();
        for (i, &id) in ids.iter().enumerate() {
            q.arm(id, base + (i as u64 + 1) * step);
        }
        q.arm(99, base + 2 * step);
        q.cancel(99); // this one must never fire
        order.clear();
        let mut firings: [Firing; 3] = core::array::from_fn(|i| Firing {
            deadline: base + (i as u64 + 1) * step,
            previous_poll: base,
            fired_at: None,
        });
        let mut fired = alloc::vec::Vec::new();
        let mut last_poll = base;
        let start = cpu::rdtsc();
        while !q.is_empty() && cpu::rdtsc().wrapping_sub(start) < tsc_hz {
            let now = time::monotonic_nanos();
            if q.expire(now, &mut fired) > 0 {
                for id in fired.drain(..) {
                    if let Some(firing) = firings.get_mut(order.len()) {
                        firing.previous_poll = last_poll;
                        firing.fired_at = Some(now);
                    }
                    order.push(id);
                }
            }
            last_poll = now;
            core::hint::spin_loop();
        }
        // A timer the queue still holds was last looked at by the final poll.
        for firing in firings.iter_mut().filter(|f| f.fired_at.is_none()) {
            firing.previous_poll = last_poll;
        }
        worst_late = firings.iter().filter_map(Firing::latest).max().unwrap_or(0);
        verdict = if ids.starts_with(&order) {
            judge(&firings, LATENESS_LIMIT)
        } else {
            // A stall cannot reorder the queue or fire a cancelled timer.
            Verdict::Late
        };
    }
    let ok = verdict == Verdict::OnTime && order == ids;
    let _ = writeln!(
        com1,
        "origin-kernel: timers: 3 deadlines (1 cancelled) fired in order {order:?}, worst lateness {} us{}{}",
        worst_late / 1000,
        if attempts > 1 {
            " (after a stalled run)"
        } else {
            ""
        },
        if ok { "" } else { ": timers failed" }
    );
    ok
}

/// Proves the TCP connection core on the heap: a loopback connection establishes,
/// carries a payload and closes (CLAUDE.md §54).
fn prove_tcp(com1: &mut console::Console) -> bool {
    let payload = b"origin-os tcp loopback payload";
    let ok = crate::tcp::loopback_demo(payload);
    let _ = writeln!(
        com1,
        "origin-kernel: tcp: loopback connection established, {}-byte payload delivered, and closed: {ok}{}",
        payload.len(),
        if ok { "" } else { ": tcp failed" }
    );
    ok
}

/// Proves the network packet layer on the heap: builds an Ethernet/IPv4/ICMP echo request
/// from host A to host B, has B parse it and build a reply, and checks A matches the reply
/// to its request with the payload intact and the checksums valid (CLAUDE.md §54).
fn prove_net(com1: &mut console::Console) -> bool {
    use crate::net::{
        EthFrame, IcmpEcho, Ipv4, Ipv4Packet, Mac, Udp, ethertype, icmp_type, protocol,
    };
    let a_mac = Mac([0x52, 0x54, 0, 0, 0, 0x0A]);
    let b_mac = Mac([0x52, 0x54, 0, 0, 0, 0x0B]);
    let (a_ip, b_ip) = (Ipv4([10, 0, 0, 10]), Ipv4([10, 0, 0, 11]));
    let payload = b"origin-os over the wire";
    let mut req = alloc::vec![0u8; 256];
    let built = (|| {
        let mut icmp = [0u8; 64];
        let n = IcmpEcho::write(icmp_type::ECHO_REQUEST, 0x4242, 1, payload, &mut icmp)?;
        let eth = EthFrame::write_header(b_mac, a_mac, ethertype::IPV4, &mut req)?;
        let ip = Ipv4Packet::write(
            a_ip,
            b_ip,
            protocol::ICMP,
            64,
            1,
            &icmp[..n],
            &mut req[eth..],
        )?;
        Some(eth + ip)
    })();
    let Some(len) = built else {
        let _ = writeln!(
            com1,
            "origin-kernel: net: cannot build the request: net failed"
        );
        return false;
    };
    // Host B receives and replies; host A checks the reply.
    let ok = (|| {
        let eth = EthFrame::parse(req.get(..len)?)?;
        if eth.dst != b_mac || eth.ethertype != ethertype::IPV4 {
            return None;
        }
        let packet = Ipv4Packet::parse(eth.payload)?;
        let echo = IcmpEcho::parse(packet.payload)?;
        if echo.kind != icmp_type::ECHO_REQUEST {
            return None;
        }
        let mut r_icmp = [0u8; 64];
        let rn = echo.reply_into(&mut r_icmp)?;
        let mut r = alloc::vec![0u8; 256];
        let reth = EthFrame::write_header(a_mac, b_mac, ethertype::IPV4, &mut r)?;
        let rip = Ipv4Packet::write(
            b_ip,
            a_ip,
            protocol::ICMP,
            64,
            2,
            &r_icmp[..rn],
            &mut r[reth..],
        )?;
        let reply_eth = EthFrame::parse(r.get(..reth + rip)?)?;
        let reply_ip = Ipv4Packet::parse(reply_eth.payload)?;
        let reply_echo = IcmpEcho::parse(reply_ip.payload)?;
        Some(
            reply_eth.dst == a_mac
                && reply_ip.src == b_ip
                && reply_ip.dst == a_ip
                && reply_echo.kind == icmp_type::ECHO_REPLY
                && reply_echo.identifier == 0x4242
                && reply_echo.data == payload,
        )
    })()
    .unwrap_or(false);
    // A UDP datagram round-trip, checksum and all.
    let mut udp = [0u8; 64];
    let udp_ok = Udp::write(a_ip, b_ip, 12345, 53, b"origin udp", &mut udp)
        .and_then(|n| Udp::parse(&udp[..n], a_ip, b_ip))
        .is_some_and(|d| d.dst_port == 53 && d.payload == b"origin udp");
    let all = ok && udp_ok;
    let _ = writeln!(
        com1,
        "origin-kernel: net: {len}-byte Ethernet/IPv4/ICMP echo built, parsed and replied; a UDP datagram round-tripped {udp_ok}; payload and checksums intact {ok}{}",
        if all { "" } else { ": net failed" }
    );
    all
}

/// Renders the rasterizer's fixed demonstration scene to a heap surface and checks its
/// CRC-32 equals the constant the host test proved, so the kernel and host renderers agree
/// bit for bit (CLAUDE.md §52 the software fallback).
fn prove_raster(com1: &mut console::Console) -> bool {
    use crate::raster::{DEMO_CRC, DEMO_HEIGHT, DEMO_WIDTH, Surface, demo_scene};
    let mut buf = alloc::vec![0u32; (DEMO_WIDTH * DEMO_HEIGHT) as usize];
    let Some(mut surface) = Surface::new(&mut buf, DEMO_WIDTH, DEMO_HEIGHT, DEMO_WIDTH) else {
        let _ = writeln!(com1, "origin-kernel: raster: bad surface: raster failed");
        return false;
    };
    demo_scene(&mut surface);
    let mut running = 0xFFFF_FFFFu32;
    for px in &buf {
        running = crate::gpt::crc32_update(running, &px.to_le_bytes());
    }
    let crc = !running;
    let ok = crc == DEMO_CRC;
    let _ = writeln!(
        com1,
        "origin-kernel: raster: {DEMO_WIDTH}x{DEMO_HEIGHT} scene rendered, CRC {crc:#010x} (expected {DEMO_CRC:#010x}){}",
        if ok { "" } else { ": raster failed" }
    );
    ok
}

/// Proves the in-memory VFS on the heap: builds a small tree, writes and reads a file
/// through `..`, lists and removes, and checks the node count returns to the root alone
/// (no leak). A failure in any step shows in the reported counts.
fn prove_vfs(com1: &mut console::Console) -> bool {
    use crate::vfs::{Kind, Vfs};
    let mut fs = Vfs::new(1 << 20, 64);
    let start = fs.node_count();
    let steps = [
        fs.mkdir_all("/etc/origin").is_ok(),
        fs.create("/etc/origin/hostname").is_ok(),
        fs.write(
            "/etc/origin/hostname",
            0,
            b"origin-os
",
        )
        .is_ok(),
        fs.mkdir("/tmp").is_ok(),
    ];
    let mut buf = [0u8; 32];
    let read = fs
        .read("/etc/origin/../origin/hostname", 0, &mut buf)
        .unwrap_or(0);
    let content_ok = &buf[..read]
        == b"origin-os
";
    let listed = fs.list("/etc/origin").unwrap_or_default();
    let stat_dir = fs.stat("/etc").map(|s| s.kind) == Ok(Kind::Directory);
    // Remove everything made, and check the tree is back to the root alone.
    let removed = fs.remove("/etc/origin/hostname").is_ok()
        && fs.remove("/etc/origin").is_ok()
        && fs.remove("/etc").is_ok()
        && fs.remove("/tmp").is_ok();
    let leaked = fs.node_count() != start;
    let ok = steps.iter().all(|&s| s)
        && content_ok
        && listed == ["hostname"]
        && stat_dir
        && removed
        && !leaked;
    let _ = writeln!(
        com1,
        "origin-kernel: vfs: built /etc/origin/hostname (read back {content_ok}), listed {} entry, removed all back to {} node{}",
        listed.len(),
        fs.node_count(),
        if ok { "" } else { ": vfs failed" }
    );
    ok
}

/// The initramfs image (a ustar archive) built into the kernel. The NIC proof also fetches
/// the same file over TFTP and compares the two.
pub(crate) static INITRAMFS_IMAGE: &[u8] = include_bytes!("../assets/initramfs.tar");

/// Extracts an embedded initramfs (a ustar image) into a fresh VFS and reports it
/// (CLAUDE.md §21 initial system deployment). Proves `ustar` + `initramfs` + `vfs` working
/// end-to-end in the kernel on a real image: the entry count, a file's contents read back,
/// and that a file whose parent directory the archive never listed was still created.
fn prove_initramfs(com1: &mut console::Console) -> bool {
    let check = check_initramfs(INITRAMFS_IMAGE);
    let _ = writeln!(
        com1,
        "origin-kernel: initramfs: {} entries into the VFS; /etc/hostname {}, implicit /boot dir {}; consistent {}{}",
        check.entries,
        check.hostname,
        check.implicit_dir,
        check.ok,
        if check.ok { "" } else { ": initramfs failed" }
    );
    check.ok
}

/// What extracting an initramfs image into a fresh VFS showed.
pub(crate) struct InitramfsCheck {
    pub(crate) entries: usize,
    pub(crate) hostname: bool,
    pub(crate) implicit_dir: bool,
    pub(crate) ok: bool,
}

/// Extracts `image` (a ustar initramfs) into a fresh VFS and checks the result: four
/// entries, `/etc/hostname`'s contents, and `/boot/banner.txt`, whose parent directory the
/// archive never lists, so it must have been created implicitly. Used for the embedded image
/// and for the copy fetched over the network.
pub(crate) fn check_initramfs(image: &[u8]) -> InitramfsCheck {
    use crate::vfs::Vfs;

    let mut fs = Vfs::new(1 << 20, 256);
    let result = crate::initramfs::extract(image, &mut fs);
    let entries = result.unwrap_or(0);
    let mut buf = [0u8; 32];
    let read = fs.read("/etc/hostname", 0, &mut buf).unwrap_or(0);
    let hostname = &buf[..read] == b"origin-os\n";
    let implicit_dir = fs.resolve("/boot/banner.txt").is_ok();
    InitramfsCheck {
        entries,
        hostname,
        implicit_dir,
        ok: result.is_ok() && entries == 4 && hostname && implicit_dir,
    }
}

/// Reports the CPU topology each online processor read about itself from CPUID, package
/// down to thread, and checks it against the ACPI MADT (CLAUDE.md §24, §27; research ledger
/// `docs/research/2026-11-08-cpu-topology-leaf-1fh.md`).
///
/// Every processor recorded its own leaves on itself (a hybrid part's differ between
/// processors), before it reported online. The checks:
/// - each processor's own CPUID x2APIC ID is the one the MADT lists for its CPU index;
/// - every processor decoded its leaves, and they all draw the same layout (the thread
///   field may differ only on a hybrid part);
/// - a hybrid processor reports a core type (the SDM makes leaf 1Ah present on all of them);
/// - no two processors have one placement;
/// - and the layout is applied to everything the MADT lists, so `cargo xtask` can compare
///   the resulting package, die, module and core counts with the machine it asked QEMU for.
///
/// None of this reads the QEMU machine definition; the counts are only printed here.
fn prove_topology(com1: &mut console::Console, rsdp: u64, online: &smp::Online) -> bool {
    use crate::topology::{self, CpuTopology, Layout, Source};

    // The boot processor's own record; every AP wrote its own before it came online.
    smp::record_topology();
    // The cache hierarchy from leaf 04h.
    let mut caches = [crate::cache::Cache {
        level: 0,
        kind: crate::cache::CacheType::Data,
        size: 0,
        line_size: 0,
        ways: 0,
        shared_by: 0,
    }; 8];
    let mut cache_subs = alloc::vec::Vec::new();
    if cpu::cpuid(0)[0] >= 4 {
        for i in 0..8u32 {
            let sub = cpu::cpuid_subleaf(4, i);
            if sub[0] & 0x1F == 0 {
                break;
            }
            cache_subs.push(sub);
        }
    }
    let n_cache = crate::cache::decode_all(&cache_subs, &mut caches);
    let caches_ok = caches[..n_cache]
        .iter()
        .all(|c| c.size > 0 && c.line_size > 0);
    let _ = write!(com1, "origin-kernel: cache: {n_cache} caches");
    for c in &caches[..n_cache] {
        let _ = write!(
            com1,
            "; L{} {:?} {} KiB (shared by {})",
            c.level,
            c.kind,
            c.size >> 10,
            c.shared_by
        );
    }
    let _ = writeln!(com1, "; consistent {caches_ok}");

    // Each online processor: the APIC ID the MADT gave its CPU index, and what it recorded.
    let records: alloc::vec::Vec<(u32, Option<CpuTopology>)> = online
        .cpus()
        .iter()
        .map(|&c| (c.apic_id().unwrap_or(u32::MAX), smp::topology_of(c)))
        .collect();
    let boot = records.first().and_then(|r| r.1);
    let source = boot.map_or(Source::None, |b| b.source);
    let layout = boot.map_or(Layout::FLAT, |b| b.layout);
    let _ = writeln!(
        com1,
        "origin-kernel: topology: layout from {source}: {layout}"
    );
    for (c, (_, own)) in online.cpus().iter().zip(&records) {
        match own {
            Some(own) => {
                let _ = write!(
                    com1,
                    "origin-kernel: topology: {}: {}",
                    c.named(),
                    own.place()
                );
                if let Some(core) = own.core {
                    let _ = write!(com1, "; core type {:#x}", core.core_type);
                }
                let _ = writeln!(com1);
            }
            None => {
                let _ = writeln!(
                    com1,
                    "origin-kernel: topology: {}: it recorded nothing about itself",
                    c.named()
                );
            }
        }
    }

    // The layout over everything the MADT lists. Counts only: xtask knows the machine.
    let listed = smp::listed_apic_ids(rsdp);
    let madt_read = listed.is_ok();
    match listed {
        Ok(mut ids) => {
            let census = layout.census(&mut ids);
            let _ = writeln!(
                com1,
                "origin-kernel: topology: the MADT lists {} processors: {} packages, {} dies, {} modules, {} cores, {} threads",
                ids.len(),
                census.packages,
                census.dies,
                census.modules,
                census.cores,
                census.threads
            );
        }
        Err(why) => {
            let _ = writeln!(
                com1,
                "origin-kernel: topology: the MADT is unreadable: {why}"
            );
        }
    }

    // Core types: only what the processors themselves report (§24: capabilities, never
    // marketing names).
    let reporting = records
        .iter()
        .filter(|r| r.1.is_some_and(|o| o.core.is_some()))
        .count();
    let hybrid = records.iter().any(|r| r.1.is_some_and(|o| o.hybrid));
    let _ = writeln!(
        com1,
        "origin-kernel: topology: core types: reported by {reporting} of {} processors; hybrid {hybrid}",
        records.len()
    );

    // The checks.
    let agreement = topology::agree(&records);
    let mut online_ids: alloc::vec::Vec<u32> = records.iter().map(|r| r.0).collect();
    let counts = layout.census(&mut online_ids);
    let consistent = agreement.holds() && madt_read && counts.threads == records.len();
    let _ = writeln!(
        com1,
        "origin-kernel: topology: against the MADT: {} processors checked, {} recorded nothing, {} have another APIC ID, {} another layout, {} an undecodable leaf, {} no core type although hybrid, {} a duplicate placement; consistent {consistent}",
        agreement.checked,
        agreement.missing,
        agreement.wrong_apic_id,
        agreement.layout_differs,
        agreement.undecodable,
        agreement.unidentified,
        agreement.duplicates,
    );
    let _ = writeln!(
        com1,
        "origin-kernel: topology: {} logical, {} cores, {} packages ({} threads/core, {} logical/package); consistent {}{}",
        counts.threads,
        counts.cores,
        counts.packages,
        layout.threads_per_core(),
        layout.logical_per_package(),
        consistent,
        if consistent { "" } else { ": topology failed" }
    );
    consistent
}

/// Reports NUMA proximity domains from the ACPI SRAT (CLAUDE.md §24 NUMA topology, §28 NUMA
/// policies). When the firmware supplies no SRAT (a uniform-memory machine, or QEMU without
/// `-numa`), that is reported as a single implicit node. Otherwise every online processor
/// must appear as an enabled CPU affinity and some memory must be present, or the line reads
/// `consistent false`.
fn prove_numa(com1: &mut console::Console, rsdp: u64, ids: &[u32]) -> bool {
    use crate::acpi::{self, CpuAffinity, MemoryAffinity};

    let Ok(srat) = crate::firmware::acpi_table(rsdp, b"SRAT") else {
        let _ = writeln!(
            com1,
            "origin-kernel: numa: no SRAT (1 node assumed); consistent true"
        );
        return true;
    };

    // The SRAT lists every processor the platform may have (QEMU: all of `maxcpus`, each
    // marked enabled), so the lists are sized from the table, on the heap: a CPU affinity
    // structure is at least 16 bytes long, a memory affinity structure 40.
    let mut cpus = alloc::vec![CpuAffinity::default(); srat.len() / 16];
    let mut mems = alloc::vec![MemoryAffinity::default(); srat.len() / 40];
    let (Ok(ncpu), Ok(nmem)) = (
        acpi::srat_cpu_affinities(srat, &mut cpus),
        acpi::srat_memory_affinities(srat, &mut mems),
    ) else {
        let _ = writeln!(
            com1,
            "origin-kernel: numa: invalid SRAT; consistent false: numa failed"
        );
        return false;
    };
    let cpus = &cpus[..ncpu];
    let mems = &mems[..nmem];

    // Distinct enabled proximity domains across both kinds of affinity.
    let mut nodes = alloc::vec::Vec::new();
    for d in cpus
        .iter()
        .filter(|c| c.enabled)
        .map(|c| c.proximity_domain)
        .chain(
            mems.iter()
                .filter(|m| m.enabled)
                .map(|m| m.proximity_domain),
        )
    {
        if !nodes.contains(&d) {
            nodes.push(d);
        }
    }

    let enabled_cpus = cpus.iter().filter(|c| c.enabled).count();
    // The lengths are firmware data, so the total must not wrap.
    let mem_bytes = mems
        .iter()
        .filter(|m| m.enabled)
        .fold(0u64, |sum, m| sum.saturating_add(m.length));
    // Every online processor must have an enabled affinity entry.
    let all_online_placed = ids
        .iter()
        .all(|id| cpus.iter().any(|c| c.enabled && c.apic_id == *id));
    let consistent = !nodes.is_empty() && mem_bytes > 0 && all_online_placed;

    let _ = writeln!(
        com1,
        "origin-kernel: numa: {} nodes, {} cpus, {} memory ranges, {} MiB; consistent {}{}",
        nodes.len(),
        enabled_cpus,
        mems.iter().filter(|m| m.enabled).count(),
        mem_bytes >> 20,
        consistent,
        if consistent { "" } else { ": numa failed" }
    );
    consistent
}

/// Reports inter-node distances from the ACPI SLIT (CLAUDE.md §27 migration cost, §28 NUMA
/// policies). The diagonal must be the normalized local distance 10 and every off-diagonal
/// at least 10, or the line reads `consistent false`. A machine with no SLIT (uniform
/// distance) is reported as such.
fn prove_slit(com1: &mut console::Console, rsdp: u64) -> bool {
    let Ok(table) = crate::firmware::acpi_table(rsdp, b"SLIT") else {
        let _ = writeln!(
            com1,
            "origin-kernel: slit: no SLIT (uniform distance assumed); consistent true"
        );
        return true;
    };
    let Ok(slit) = crate::acpi::parse_slit(table) else {
        let _ = writeln!(
            com1,
            "origin-kernel: slit: invalid SLIT; consistent false: slit failed"
        );
        return false;
    };

    let n = slit.localities();
    let mut local_ok = true;
    let mut off_ok = true;
    for i in 0..n {
        for j in 0..n {
            let d = slit.distance(i, j).unwrap_or(0);
            if i == j {
                local_ok &= d == 10;
            } else {
                off_ok &= d >= 10;
            }
        }
    }
    // The locality map derived from this SLIT: every node's nearest node must be itself,
    // and node 0's nodes in ascending distance.
    let self_nearest = crate::numa::self_is_nearest(&slit);
    let mut order = [0usize; 64];
    let k = crate::numa::nearest_nodes(&slit, 0, &mut order);
    let consistent = n >= 1 && local_ok && off_ok && self_nearest;
    let across = slit.distance(0, 1.min(n.saturating_sub(1))).unwrap_or(0);
    let _ = write!(
        com1,
        "origin-kernel: slit: {n} localities, local 10 ({local_ok}), 0->last {across}, self-nearest {self_nearest}, from 0 ["
    );
    for (i, node) in order[..k].iter().enumerate() {
        let _ = write!(com1, "{}{}", if i == 0 { "" } else { ", " }, node);
    }
    let _ = writeln!(
        com1,
        "]; consistent {}{}",
        consistent,
        if consistent { "" } else { ": slit failed" }
    );
    consistent
}

/// Proves the kernel's hardware entropy source (CLAUDE.md §59 secure randomness). Draws a
/// batch of 64-bit values through the bounded-retry logic in [`framekernel::entropy`] over
/// RDSEED (preferred) and RDRAND, and checks they all arrived, are non-zero and are
/// distinct — the smoke test that distinguishes a live entropy source from a dead one that
/// returns a constant. A processor that reports neither instruction is reported honestly,
/// not failed.
fn prove_entropy(com1: &mut console::Console, features: &crate::features::Features) -> bool {
    use crate::entropy::{self, Source};

    if !features.rdseed && !features.rdrand {
        let _ = writeln!(
            com1,
            "origin-kernel: entropy: no RDSEED or RDRAND; consistent true (no source claimed)"
        );
        return true;
    }

    // Prefer RDSEED (full-entropy), falling back to RDRAND within one attempt; the generic
    // `draw` loop then retries this combined source per Intel's budget.
    struct Hw {
        rdseed: bool,
        rdrand: bool,
        from_rdseed: u32,
        from_rdrand: u32,
    }
    impl Source for Hw {
        fn try_next(&mut self) -> Option<u64> {
            if self.rdseed
                && let Some(v) = cpu::rdseed()
            {
                self.from_rdseed += 1;
                return Some(v);
            }
            if self.rdrand
                && let Some(v) = cpu::rdrand()
            {
                self.from_rdrand += 1;
                return Some(v);
            }
            None
        }
    }

    let mut src = Hw {
        rdseed: features.rdseed,
        rdrand: features.rdrand,
        from_rdseed: 0,
        from_rdrand: 0,
    };
    let mut values = [0u64; 16];
    let mut drawn = 0;
    for slot in &mut values {
        match entropy::draw(&mut src, entropy::RDRAND_RETRIES) {
            Some(v) => {
                *slot = v;
                drawn += 1;
            }
            None => break,
        }
    }
    let all = drawn == values.len();
    let nonzero = values[..drawn].iter().all(|&v| v != 0);
    let distinct = values[..drawn]
        .iter()
        .enumerate()
        .all(|(i, &v)| !values[..i].contains(&v));
    let consistent = all && nonzero && distinct;
    let _ = writeln!(
        com1,
        "origin-kernel: entropy: RDSEED {}, RDRAND {}; {drawn} draws ({} RDSEED, {} RDRAND), all {all}, distinct {distinct}, nonzero {nonzero}; consistent {}{}",
        features.rdseed,
        features.rdrand,
        src.from_rdseed,
        src.from_rdrand,
        consistent,
        if consistent { "" } else { ": entropy failed" }
    );
    consistent
}

/// A three-deep call chain whose innermost frame captures a backtrace; `#[inline(never)]`
/// so the frames really exist.
#[inline(never)]
fn backtrace_c(out: &mut [u64]) -> usize {
    core::hint::black_box(cpu::capture_backtrace(out))
}
#[inline(never)]
fn backtrace_b(out: &mut [u64]) -> usize {
    core::hint::black_box(backtrace_c(out))
}
#[inline(never)]
fn backtrace_a(out: &mut [u64]) -> usize {
    core::hint::black_box(backtrace_b(out))
}

/// Proves the frame-pointer backtrace (CLAUDE.md §69): a known call chain must produce at
/// least four return addresses, every one inside the kernel image's 2 GiB virtual window.
fn prove_backtrace(com1: &mut Console, info: &BootInfo) -> bool {
    let mut frames = [0u64; 16];
    let n = backtrace_a(&mut frames);
    let base = info.kernel_virtual_base;
    // The kernel image occupies the top 2 GiB (ADR-0004 H2), so every return address into
    // kernel code is at or above its virtual base (the image's first byte).
    let in_kernel = frames.iter().take(n).all(|&a| a >= base);
    let ok = n >= 4 && in_kernel;
    let _ = writeln!(
        com1,
        "origin-kernel: backtrace: {n} frames from a known call chain, all in the kernel image {}{}",
        if in_kernel { "true" } else { "false" },
        if ok { "" } else { ": backtrace failed" }
    );
    ok
}

/// The kernel's page tables, built, the framebuffer kept, loaded and proven. Returns whether
/// the kernel now runs on them.
fn init_paging(com1: &mut Console, early: &mut Early) -> bool {
    let Some(mut tables) = early.build_kernel_tables(com1) else {
        return false;
    };
    // The console keeps drawing through the switch to these tables, at the address the
    // loader used (ADR-0010). Last, so that it costs nothing else if it fails.
    if console::has_screen() && !early.map_framebuffer(&mut tables) {
        console::withdraw_framebuffer(com1);
    }
    early.load_kernel_tables(com1, tables)
}

/// Proves the heap with real `alloc` collections: a large vector (a frame run), thousands
/// of boxes (slabs), a B-tree map and a string, with their contents checked; then that
/// dropping them all returns the heap and the frame allocator exactly to where they were.
fn prove_heap(com1: &mut Console) -> bool {
    use alloc::boxed::Box;
    use alloc::collections::BTreeMap;
    use alloc::string::String;
    use alloc::vec::Vec;

    let Some(before) = memory::stats() else {
        return false;
    };
    let peak = {
        let numbers: Vec<u64> = (0..100_000u64).collect();
        let sum: u64 = numbers.iter().sum();
        let boxes: Vec<Box<[u64; 4]>> = (0..2000u64).map(|i| Box::new([i; 4])).collect();
        let box_sum: u64 = boxes.iter().map(|b| b[3]).sum();
        let mut map = BTreeMap::new();
        for i in 0..1000u64 {
            map.insert(i * 7 % 1000, i);
        }
        let mut text = String::new();
        let _ = write!(text, "{sum}");
        let right = sum == 4_999_950_000
            && box_sum == 1_999_000
            && map.len() == 1000
            && map.keys().copied().eq(0..1000)
            && text == "4999950000";
        if !right {
            let _ = writeln!(com1, "origin-kernel: heap contents are wrong");
            return false;
        }
        memory::stats()
    };
    let after = memory::stats();
    if let Some((p, _)) = peak {
        let _ = writeln!(
            com1,
            "origin-kernel: heap: Vec of 100000 u64, 2000 boxes, B-tree of 1000 keys, a string: {} KiB in use, {} slabs, {} pages in runs",
            p.bytes_in_use >> 10,
            p.slabs,
            p.large_pages
        );
    }
    if after != Some(before) {
        let _ = writeln!(
            com1,
            "origin-kernel: heap leaked: {after:?} after, {before:?} before"
        );
        return false;
    }
    let _ = writeln!(
        com1,
        "origin-kernel: heap: everything freed; {} free frames ({} MiB), as before",
        before.1,
        (before.1 * memory::FRAME) >> 20
    );
    true
}

/// What sending this processor an NMI found.
struct NmiProof {
    /// One arrived within the budget.
    delivered: bool,
    /// It ran on the dedicated NMI stack (IST2), not on the interrupted stack.
    on_own_stack: bool,
}

/// Sends this processor an NMI and checks where it was taken. An NMI that does not arrive
/// says nothing about the stack: that case, and a wrong stack, stay apart in the result.
fn prove_nmi_ist(com1: &mut Console, tsc_hz: u64) -> NmiProof {
    let (before, _) = cpu::nmis();
    timer::send_nmi_to_self();
    let start = cpu::rdtsc();
    while cpu::nmis().0 == before && cpu::rdtsc().wrapping_sub(start) < tsc_hz {
        core::hint::spin_loop();
    }
    let (count, frame) = cpu::nmis();
    if count == before {
        let _ = writeln!(
            com1,
            "origin-kernel: NMI: none arrived within a second of sending one: NMI stack failed"
        );
        return NmiProof {
            delivered: false,
            on_own_stack: false,
        };
    }
    let (low, high) = cpu::bsp_nmi_stack();
    let on_own_stack = (low..high).contains(&frame);
    let _ = writeln!(
        com1,
        "origin-kernel: NMI taken on its own stack (IST2): frame at {frame:#x}, stack {low:#x}..{high:#x}{}",
        if on_own_stack {
            ""
        } else {
            ": NMI stack failed"
        }
    );
    NmiProof {
        delivered: true,
        on_own_stack,
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if !PANICKING.swap(true, Ordering::AcqRel) {
        // The urgent console never waits long for its lock, so a panic inside the console
        // still reports; before `console::init` it programs COM1 itself.
        let mut out = console::Console::urgent();
        let _ = report::write_panic(&mut out, info);
        let mut frames = [0u64; 16];
        let n = cpu::capture_backtrace(&mut frames);
        let base = crate::phys::kernel_virtual_base();
        let _ = write!(out, "origin-kernel: backtrace (kernel base {base:#x}):");
        for addr in frames.iter().take(n) {
            let _ = write!(out, " {addr:#x}");
        }
        let _ = writeln!(out);
    }
    halt()
}
