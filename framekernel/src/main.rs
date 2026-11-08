//! `_start`, the entry point of the Origin OS Framekernel (M1).
//!
//! The Origin boot loader calls `_start` after `ExitBootServices`, with:
//! - the physical address of a [`BootInfo`] in `rdi` (System V);
//! - interrupts disabled, and the processor at CPL 0;
//! - the firmware's identity-mapped page tables still active (physical == virtual);
//! - `rsp` 16-byte aligned before the `call`, on the stack `BootInfo` describes;
//! - the direction flag clear;
//! - the image's relocations applied.
//!
//! The kernel assumes nothing else about memory. It programs COM1, checks that its own
//! relocations were applied, validates the boot info and prints a summary. Then come the
//! first M2 steps: its own GDT, TSS and IDT (proven with a breakpoint), the frame
//! allocator, and its own page tables with a W^X kernel image (proven with deliberate
//! faults), after which it halts. Only the boot processor runs, and nothing here enables
//! interrupts.
#![no_std]
#![no_main]

extern crate alloc;

use core::arch::asm;
use core::fmt::Write;
use core::panic::PanicInfo;
use core::ptr;
use core::slice;
use core::sync::atomic::{AtomicBool, Ordering};

use framekernel::boot::{self, InvalidBootInfo, MemorySummary};

use crate::console::Console;
use framekernel::report;

mod block;
mod console;
mod cpu;
mod dma;
mod firmware;
mod i8042;
mod iommu;
mod irq;
mod memory;
mod pcie;
mod phys;
mod process;
mod sched;
mod smp;
mod storage;
mod time;
mod timer;
mod tlb;
mod user;
mod virtionet;
mod vm;
mod wait;
use framekernel::frames::{self, FrameAllocator};
use framekernel::paging::layout::{self, Placement};
use framekernel::paging::{PAGE, Table, TablePool};
use framekernel::serial::{Register, Uart16550, UartRegisters};
use origin_boot_info::{
    BootInfo, KernelSegment, MemoryRegion, RegionKind, SEGMENT_EXECUTE, SEGMENT_WRITE,
};

/// Frames reserved, next to the frame bitmap, for the kernel's page tables: enough for a
/// direct map of well over 100 GiB with 2 MiB pages plus 4 KiB edges at every region.
const TABLE_POOL_FRAMES: u64 = 256;

/// I/O port base of COM1 on PC-compatible platforms.
const COM1_BASE: u16 = 0x3F8;

/// Set by the first panic. A panic while reporting a panic only halts.
static PANICKING: AtomicBool = AtomicBool::new(false);

/// The object [`RELOCATION_PROBE`] points at.
static RELOCATION_TARGET: [u8; 8] = *b"ORIGRELO";
/// A pointer stored in the image. The ELF has an `R_X86_64_RELATIVE` relocation for it, so
/// its run-time value equals the run-time address of [`RELOCATION_TARGET`] only if the
/// loader applied the image's relocations.
static RELOCATION_PROBE: &[u8; 8] = &RELOCATION_TARGET;

/// The ELF entry. A tiny naked shim so every backtrace terminates here: it zeroes RBP (so
/// the frame-pointer chain ends), aligns the stack, and calls [`kernel_main`] with the boot
/// info the loader left in RDI. It never returns.
///
/// # Safety
///
/// Only the Origin boot loader may jump here, once, under the entry contract in the module
/// documentation.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "sysv64" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor ebp, ebp",
        "and rsp, -16",
        "call {main}",
        "ud2",
        main = sym kernel_main,
    );
}

/// The kernel's Rust entry, called by [`_start`] with RBP zero and the boot-info pointer in
/// RDI.
///
/// # Safety
///
/// Called once by `_start`, under the entry contract in the module documentation.
/// `boot_info` must be null or the address of a readable `BootInfo` whose region array, if
/// the header is valid, is readable and is not written while the kernel runs.
unsafe extern "sysv64" fn kernel_main(boot_info: *const BootInfo) -> ! {
    // The monotonic clock's zero (CLAUDE.md §68).
    time::mark_entry();
    // SAFETY: the entry contract: CPL 0, boot processor only, interrupts disabled. The
    // firmware and the loader stopped using COM1 at `ExitBootServices`, and no other kernel
    // code drives it.
    let mut uart = Uart16550::new(unsafe { Com1::new() });
    uart.init();

    if !relocations_applied() {
        // `core::fmt` would call through unrelocated vtables. `write_str` on the concrete
        // type is a direct call, and the string is addressed RIP-relative.
        let _ = uart.write_str(report::RELOCATIONS_NOT_APPLIED);
        halt();
    }
    // From here on every processor prints through the console, a line at a time.
    console::init(uart);
    let mut com1 = console::Console::new();

    // SAFETY: `boot_info` is the loader's pointer, passed on unchanged, and this function's
    // contract is exactly `read_boot_info`'s.
    let (info, regions, segments, memory) = match unsafe { read_boot_info(boot_info) } {
        Ok(v) => v,
        Err(err) => {
            let _ = report::write_invalid(&mut com1, err);
            halt();
        }
    };
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

    // M2, first step: the kernel's own GDT, TSS and IDT, proven with a breakpoint whose
    // handler reports it and resumes here.
    // SAFETY: called once, at CPL 0, on the boot processor, with interrupts disabled (entry
    // contract), before anything depends on the firmware's tables.
    unsafe { cpu::init() };
    let _ = report::write_cpu_tables(&mut com1);
    cpu::breakpoint();

    // CPU features and the x86-64-v3 baseline (CLAUDE.md §14, §24), then SSE/AVX enabled on
    // the boot processor.
    let cpuid = cpu::gather_cpuid();
    let features = framekernel::features::Features::decode(&cpuid);
    // SAFETY: CPL 0, boot processor, once, before any SIMD runs on it.
    let xcr0 = unsafe { cpu::enable_simd(&cpuid) };
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
    prove_backtrace(&mut com1, &info);

    // M2, second step: the physical frame allocator over the boot-info regions, with a
    // pool of frames for page tables beside its bitmap.
    let Some((mut alloc, pool, pool_base)) = init_memory(&mut com1, regions) else {
        halt();
    };

    // M2, third step: the kernel's own page tables, proven with deliberate faults; then
    // the frame allocator is proven again through the new direct map.
    let mut heap_ok = false;
    if init_paging(&mut com1, &info, regions, segments, pool, pool_base) {
        prove_frames(&mut com1, &mut alloc);
        // M2: the kernel heap, which takes over the frame allocator; from here on the
        // `alloc` crate works in the kernel.
        memory::init(alloc);
        heap_ok = prove_heap(&mut com1);
        if heap_ok {
            prove_user_isolation(&mut com1);
        }
    }

    // M2, fourth step: interrupts. The local APIC in x2APIC mode, its timer calibrated
    // against the PIT and proven by taking periodic interrupts.
    let tsc_hz = timer::init(&mut com1);
    if let Some(hz) = tsc_hz {
        prove_nmi_ist(&mut com1);
        // The clocks: the monotonic clock's TSC rate, proven against the RTC, and the wall
        // clock from the RTC (CLAUDE.md §68).
        time::init(&mut com1, hz, info.acpi_rsdp);
        // I/O APIC interrupt routing, proven by routing the PIT's IRQ0 (CLAUDE.md §24).
        // SAFETY: the BSP's APIC is in x2APIC mode (timer::init) and the PICs are masked.
        let bsp = unsafe { timer::X2Apic::enable() }.read(timer::ApicRead::Id) as u32;
        irq::prove(&mut com1, info.acpi_rsdp, bsp, hz);
    }

    // M3, first step: start the application processors the ACPI MADT lists. It needs the
    // heap, the kernel's own tables and the calibrated TSC.
    if let Some(tsc_hz) = tsc_hz
        && heap_ok
    {
        let online = smp::start(&mut com1, &info, regions, tsc_hz);
        // M2 completion: once every enabled processor has left firmware code, the loader's
        // and the firmware's boot-time memory belongs to the kernel.
        if online.complete()
            && let Some((added, free)) = memory::reclaim_loader_memory()
        {
            let _ = writeln!(
                com1,
                "origin-kernel: reclaimed {} MiB of loader and boot-services memory; {} MiB free",
                (added * memory::FRAME) >> 20,
                (free * memory::FRAME) >> 20
            );
        }
        // CPU topology from CPUID leaf 0Bh (CLAUDE.md §24, §27).
        let apic_ids = online.apic_ids();
        prove_topology(&mut com1, &apic_ids);
        // NUMA affinities from the ACPI SRAT (CLAUDE.md §24, §28).
        prove_numa(&mut com1, info.acpi_rsdp, &apic_ids);
        // Inter-node distances from the ACPI SLIT (CLAUDE.md §27, §28).
        prove_slit(&mut com1, info.acpi_rsdp);
        // Hardware entropy from RDSEED/RDRAND (CLAUDE.md §59).
        prove_entropy(&mut com1, &features);
        // M3, second step: preemptive kernel threads on every online processor.
        sched::prove(&mut com1, online.cpus(), tsc_hz);
        // M5 groundwork: PCI Express enumeration through the ACPI MCFG.
        if pcie::init(&mut com1, info.acpi_rsdp, regions, online.cpus().len()) {
            // DMA and interrupt remapping (Intel VT-d) before any driver binds: translation
            // on with no device attached, so every DMA faults until a driver attaches its
            // device (CLAUDE.md §32, §58; ADR-0007).
            iommu::init(&mut com1, info.acpi_rsdp);
            // M5: the NVMe driver and block I/O, polled and then interrupt-driven.
            storage::init(&mut com1, online.cpus(), tsc_hz);
            // M10: bring up the virtio-net NIC; an interrupt-driven receive thread takes the
            // OS onto the network (CLAUDE.md §31, §54).
            virtionet::prove(&mut com1, online.cpus(), tsc_hz);
            // The IOMMU's own proofs, through the bound NVMe controller: translated DMA, a
            // blocked and recorded DMA, remapped and blocked interrupts.
            storage::prove_iommu(&mut com1, online.cpus(), tsc_hz);
        }
        sched::stop_boot_processor();
    }

    // M8/M9 groundwork: the in-memory VFS (paths, directories, files).
    prove_vfs(&mut com1);
    // §21 groundwork: extract an embedded initramfs (a ustar image) into a VFS.
    prove_initramfs(&mut com1);
    // M8 groundwork: the software rasterizer draws a fixed scene whose CRC must match the
    // host-proven one, bit for bit.
    prove_raster(&mut com1);
    // M10 groundwork: the i8042 PS/2 controller bring-up (CLAUDE.md §56).
    i8042::init(&mut com1);
    // Networking groundwork: build and parse an ICMP echo through a loopback (CLAUDE.md §54).
    prove_net(&mut com1);
    // TCP: a full connection over an in-memory loopback (CLAUDE.md §54).
    prove_tcp(&mut com1);
    // The monotonic timer queue, driven by the real clock (CLAUDE.md §68).
    prove_timers(&mut com1);
    // Integration: a read-only /sys tree in the VFS carrying live discovered facts.
    prove_sysfs(&mut com1);

    let _ = report::write_halt(&mut com1);
    halt()
}

/// Builds a read-only `/sys`-style tree in the VFS from facts several subsystems
/// discovered (free memory, the TSC rate, the PCIe device count, the NVMe disk size),
/// then reads values back and checks they parse to the discovered numbers. This shows the
/// VFS carrying a live system namespace built from real hardware state.
fn prove_sysfs(com1: &mut console::Console) {
    use framekernel::vfs::Vfs;
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
fn prove_timers(com1: &mut console::Console) {
    use framekernel::timers::TimerQueue;
    let mut q = TimerQueue::new();
    let base = time::monotonic_nanos();
    let step = 5_000_000u64; // 5 ms
    let ids = [10u64, 20, 30];
    for (i, &id) in ids.iter().enumerate() {
        q.arm(id, base + (i as u64 + 1) * step);
    }
    q.arm(99, base + 2 * step);
    q.cancel(99); // this one must never fire
    let mut order = alloc::vec::Vec::new();
    let mut fired = alloc::vec::Vec::new();
    let mut worst_late = 0u64;
    let start = cpu::rdtsc();
    let tsc_hz = time::tsc_hz();
    while !q.is_empty() && cpu::rdtsc().wrapping_sub(start) < tsc_hz {
        let now = time::monotonic_nanos();
        if q.expire(now, &mut fired) > 0 {
            for id in fired.drain(..) {
                let deadline = base + (order.len() as u64 + 1) * step;
                worst_late = worst_late.max(now.saturating_sub(deadline));
                order.push(id);
            }
        }
        core::hint::spin_loop();
    }
    let ok = order == ids && worst_late < 2_000_000; // within 2 ms
    let _ = writeln!(
        com1,
        "origin-kernel: timers: 3 deadlines (1 cancelled) fired in order {order:?}, worst lateness {} us{}",
        worst_late / 1000,
        if ok { "" } else { ": timers failed" }
    );
}

/// Proves the TCP connection core on the heap: a loopback connection establishes,
/// carries a payload and closes (CLAUDE.md §54).
fn prove_tcp(com1: &mut console::Console) {
    let payload = b"origin-os tcp loopback payload";
    let ok = framekernel::tcp::loopback_demo(payload);
    let _ = writeln!(
        com1,
        "origin-kernel: tcp: loopback connection established, {}-byte payload delivered, and closed: {ok}{}",
        payload.len(),
        if ok { "" } else { ": tcp failed" }
    );
}

/// Proves the network packet layer on the heap: builds an Ethernet/IPv4/ICMP echo request
/// from host A to host B, has B parse it and build a reply, and checks A matches the reply
/// to its request with the payload intact and the checksums valid (CLAUDE.md §54).
fn prove_net(com1: &mut console::Console) {
    use framekernel::net::{
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
        return;
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
}

/// Renders the rasterizer's fixed demonstration scene to a heap surface and checks its
/// CRC-32 equals the constant the host test proved, so the kernel and host renderers agree
/// bit for bit (CLAUDE.md §52 the software fallback).
fn prove_raster(com1: &mut console::Console) {
    use framekernel::raster::{DEMO_CRC, DEMO_HEIGHT, DEMO_WIDTH, Surface, demo_scene};
    let mut buf = alloc::vec![0u32; (DEMO_WIDTH * DEMO_HEIGHT) as usize];
    let Some(mut surface) = Surface::new(&mut buf, DEMO_WIDTH, DEMO_HEIGHT, DEMO_WIDTH) else {
        let _ = writeln!(com1, "origin-kernel: raster: bad surface: raster failed");
        return;
    };
    demo_scene(&mut surface);
    let mut running = 0xFFFF_FFFFu32;
    for px in &buf {
        running = framekernel::gpt::crc32_update(running, &px.to_le_bytes());
    }
    let crc = !running;
    let ok = crc == DEMO_CRC;
    let _ = writeln!(
        com1,
        "origin-kernel: raster: {DEMO_WIDTH}x{DEMO_HEIGHT} scene rendered, CRC {crc:#010x} (expected {DEMO_CRC:#010x}){}",
        if ok { "" } else { ": raster failed" }
    );
}

/// Proves the in-memory VFS on the heap: builds a small tree, writes and reads a file
/// through `..`, lists and removes, and checks the node count returns to the root alone
/// (no leak). A failure in any step shows in the reported counts.
fn prove_vfs(com1: &mut console::Console) {
    use framekernel::vfs::{Kind, Vfs};
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
}

/// The initramfs image (a ustar archive) built into the kernel. The NIC proof also fetches
/// the same file over TFTP and compares the two.
pub(crate) static INITRAMFS_IMAGE: &[u8] = include_bytes!("../assets/initramfs.tar");

/// Extracts an embedded initramfs (a ustar image) into a fresh VFS and reports it
/// (CLAUDE.md §21 initial system deployment). Proves `ustar` + `initramfs` + `vfs` working
/// end-to-end in the kernel on a real image: the entry count, a file's contents read back,
/// and that a file whose parent directory the archive never listed was still created.
fn prove_initramfs(com1: &mut console::Console) {
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
    use framekernel::vfs::Vfs;

    let mut fs = Vfs::new(1 << 20, 256);
    let result = framekernel::initramfs::extract(image, &mut fs);
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

/// Reports the CPU topology from CPUID leaf 0Bh and checks that the online processors'
/// x2APIC IDs decompose consistently (every package/core/thread triple unique), the
/// groundwork for topology-aware scheduling (CLAUDE.md §24, §27).
fn prove_topology(com1: &mut console::Console, ids: &[u32]) {
    use framekernel::topology::Topology;
    // Read leaf 0Bh sub-leaves until one reports level type 0 (or four, enough for SMT and
    // core).
    let mut subs = [[0u32; 4]; 4];
    let mut n = 0;
    if cpu::cpuid(0)[0] >= 0x0B {
        for (i, slot) in subs.iter_mut().enumerate() {
            *slot = cpu::cpuid_subleaf(0x0B, i as u32);
            n = i + 1;
            if (slot[2] >> 8) & 0xFF == 0 {
                break;
            }
        }
    }
    let topology = Topology::decode(&subs[..n]);
    let mut scratch = [(0u32, 0u32, 0u32); crate::cpu::MAX_CPUS];
    let (counts, consistent) = framekernel::topology::analyze(&topology, ids, &mut scratch);
    // The cache hierarchy from leaf 04h.
    let mut caches = [framekernel::cache::Cache {
        level: 0,
        kind: framekernel::cache::CacheType::Data,
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
    let n_cache = framekernel::cache::decode_all(&cache_subs, &mut caches);
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

    let _ = writeln!(
        com1,
        "origin-kernel: topology: {} logical, {} cores, {} packages ({} threads/core, {} logical/package); consistent {}{}",
        counts.threads,
        counts.cores,
        counts.packages,
        topology.threads_per_core(),
        topology.logical_per_package(),
        consistent,
        if consistent { "" } else { ": topology failed" }
    );
}

/// Reports NUMA proximity domains from the ACPI SRAT (CLAUDE.md §24 NUMA topology, §28 NUMA
/// policies). When the firmware supplies no SRAT (a uniform-memory machine, or QEMU without
/// `-numa`), that is reported as a single implicit node. Otherwise every online processor
/// must appear as an enabled CPU affinity and some memory must be present, or the line reads
/// `consistent false`.
fn prove_numa(com1: &mut console::Console, rsdp: u64, ids: &[u32]) {
    use framekernel::acpi::{self, CpuAffinity, MemoryAffinity};

    let Ok(srat) = crate::firmware::acpi_table(rsdp, b"SRAT") else {
        let _ = writeln!(
            com1,
            "origin-kernel: numa: no SRAT (1 node assumed); consistent true"
        );
        return;
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
        return;
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
    let mem_bytes: u64 = mems.iter().filter(|m| m.enabled).map(|m| m.length).sum();
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
}

/// Reports inter-node distances from the ACPI SLIT (CLAUDE.md §27 migration cost, §28 NUMA
/// policies). The diagonal must be the normalized local distance 10 and every off-diagonal
/// at least 10, or the line reads `consistent false`. A machine with no SLIT (uniform
/// distance) is reported as such.
fn prove_slit(com1: &mut console::Console, rsdp: u64) {
    let Ok(table) = crate::firmware::acpi_table(rsdp, b"SLIT") else {
        let _ = writeln!(
            com1,
            "origin-kernel: slit: no SLIT (uniform distance assumed); consistent true"
        );
        return;
    };
    let Ok(slit) = framekernel::acpi::parse_slit(table) else {
        let _ = writeln!(
            com1,
            "origin-kernel: slit: invalid SLIT; consistent false: slit failed"
        );
        return;
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
    let self_nearest = framekernel::numa::self_is_nearest(&slit);
    let mut order = [0usize; 64];
    let k = framekernel::numa::nearest_nodes(&slit, 0, &mut order);
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
}

/// Proves the kernel's hardware entropy source (CLAUDE.md §59 secure randomness). Draws a
/// batch of 64-bit values through the bounded-retry logic in [`framekernel::entropy`] over
/// RDSEED (preferred) and RDRAND, and checks they all arrived, are non-zero and are
/// distinct — the smoke test that distinguishes a live entropy source from a dead one that
/// returns a constant. A processor that reports neither instruction is reported honestly,
/// not failed.
fn prove_entropy(com1: &mut console::Console, features: &framekernel::features::Features) {
    use framekernel::entropy::{self, Source};

    if !features.rdseed && !features.rdrand {
        let _ = writeln!(
            com1,
            "origin-kernel: entropy: no RDSEED or RDRAND; consistent true (no source claimed)"
        );
        return;
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
fn prove_backtrace(com1: &mut Console, info: &BootInfo) {
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
}

/// Builds the kernel's address space, loads it, and proves its protections on the
/// processor. Returns whether the kernel now runs on its own tables.
fn init_paging(
    com1: &mut Console,
    info: &BootInfo,
    regions: &[MemoryRegion],
    segments: &[KernelSegment],
    pool: &'static [Table],
    pool_base: u64,
) -> bool {
    if !cpu::nx_supported() {
        let _ = writeln!(
            com1,
            "origin-kernel: the processor has no NX bit; W^X cannot be enforced"
        );
        return false;
    }
    let mut tables = TablePool::new(pool, pool_base);
    let stack_guard = info.stack_base.wrapping_sub(PAGE);
    let at = Placement {
        direct_map_base: info.direct_map_base,
        kernel_virtual_base: info.kernel_virtual_base,
        kernel_physical_base: info.kernel_base,
        stack_guard,
    };
    let (space, stats) = match layout::build(&mut tables, &at, regions, segments) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(com1, "origin-kernel: page tables not built: {e:?}");
            return false;
        }
    };
    // SAFETY: boot processor, CPL 0, interrupts disabled (entry contract); NX is supported
    // (checked above). `kernel_space::build` mapped the kernel image segments, the stack
    // this code runs on, the boot info and region and segment arrays, the frame bitmap and
    // this table pool (all in direct-mapped regions), and the kernel's statics (the GDT,
    // IDT, TSS and double-fault stack are in the RW segment). COM1 is port I/O.
    unsafe { cpu::load_page_tables(space.root()) };
    // SAFETY: CPL 0; the tables just loaded mark only the kernel half global (`layout`).
    // Toggling CR4.PGE also drops every translation cached under the loader's tables.
    let global = unsafe { tlb::enable_global_pages() };
    let used = tables.used();
    vm::install(space, tables);
    let _ = writeln!(
        com1,
        "origin-kernel: page tables loaded: {} tables, {} MiB direct map, {} image pages, {} 4 KiB and {} 2 MiB leaves; global pages {global}",
        used,
        stats.direct_bytes >> 20,
        stats.image_pages,
        stats.small_pages,
        stats.large_pages
    );
    // ADR-0004: nothing of the kernel is left in the lower half, and its physical pages
    // have no second mapping in the direct map.
    let identity = vm::is_mapped(info.kernel_base);
    let alias = vm::is_mapped(phys::to_virt(info.kernel_base));
    let _ = writeln!(
        com1,
        "origin-kernel: higher half: kernel image identity address mapped {identity}, direct-map alias mapped {alias}{}",
        if identity || alias {
            ": higher half failed"
        } else {
            ""
        }
    );
    !identity && !alias && prove_protections(com1, info, segments)
}

/// Deliberate accesses that the new tables must refuse: a write to kernel code, a write to
/// read-only kernel data, an instruction fetch from writable data and a read of page 0.
/// Each must raise #PF with the expected error code (SDM Vol. 3A §4.7: bit 0 present,
/// bit 1 write, bit 4 instruction fetch). Returns whether all four did.
fn prove_protections(com1: &mut Console, info: &BootInfo, segments: &[KernelSegment]) -> bool {
    let first = |want: u32, avoid: u32| {
        segments
            .iter()
            .find(|s| s.access & want == want && s.access & avoid == 0)
            .map(|s| info.kernel_virtual_base + s.rva)
    };
    let (Some(text), Some(rodata)) = (
        first(SEGMENT_EXECUTE, SEGMENT_WRITE),
        first(0, SEGMENT_WRITE | SEGMENT_EXECUTE),
    ) else {
        let _ = writeln!(com1, "origin-kernel: no code or read-only segment to probe");
        return false;
    };
    let read = |addr: u64| {
        // SAFETY: `addr` is the first byte of a mapped, readable kernel segment.
        unsafe { ptr::with_exposed_provenance::<u8>(addr as usize).read_volatile() }
    };
    let (exec_addr, exec) = cpu::probe_execute_data();
    let cases = [
        ("write to kernel code", text, 0x3, {
            let byte = read(text);
            // SAFETY: `byte` is the value at `text`, kernel code that nothing modifies, and
            // `text` is RAM, not MMIO.
            unsafe { cpu::probe_write(text, byte) }
        }),
        ("write to read-only kernel data", rodata, 0x3, {
            let byte = read(rodata);
            // SAFETY: as above, for read-only data.
            unsafe { cpu::probe_write(rodata, byte) }
        }),
        (
            "instruction fetch from writable data",
            exec_addr,
            0x11,
            exec,
        ),
        // SAFETY: page 0 is RAM or unmapped, never MMIO.
        ("read of page 0", 0, 0x0, unsafe { cpu::probe_read(0) }),
        (
            "read of the boot stack's guard page",
            phys::to_virt(info.stack_base - PAGE),
            0x0,
            // SAFETY: the guard is a RAM page of the KernelStack region, not MMIO.
            unsafe { cpu::probe_read(phys::to_virt(info.stack_base - PAGE)) },
        ),
    ];
    let mut all = true;
    for (what, addr, expected, probe) in cases {
        match probe {
            cpu::Probe::Faulted { error, address } if error == expected && address == addr => {
                let _ = writeln!(
                    com1,
                    "origin-kernel: page protection: {what} at {addr:#x} faulted (#PF error {error:#x})"
                );
            }
            other => {
                all = false;
                let _ = writeln!(
                    com1,
                    "origin-kernel: page protection violated: {what} at {addr:#x}: {other:?}"
                );
            }
        }
    }
    all
}

/// Builds the frame allocator, with its bitmap and then the page-table pool at the start of
/// the first usable region that can hold both. Returns the allocator, the pool and the
/// pool's physical address.
fn init_memory(
    com1: &mut Console,
    regions: &'static [MemoryRegion],
) -> Option<(FrameAllocator<'static>, &'static [Table], u64)> {
    let words = frames::words_for(frames::covered_end(regions));
    let bitmap_pages = (words * 8).div_ceil(frames::FRAME);
    let pages = bitmap_pages + TABLE_POOL_FRAMES;
    let usable = RegionKind::Usable as u32;
    let Some(home) = regions
        .iter()
        .find(|r| r.kind == usable && r.base != 0 && r.pages >= pages)
    else {
        let _ = writeln!(
            com1,
            "origin-kernel: no usable region can hold the {pages}-page frame bitmap and table pool"
        );
        return None;
    };
    let bitmap_ptr = ptr::with_exposed_provenance_mut::<u64>(phys::to_virt(home.base) as usize);
    // SAFETY: `home` is a `Usable` region from the validated boot info, so nothing else
    // owns it; the firmware's identity mapping is active, so its physical address is
    // directly writable; it holds at least `pages` pages, which cover the bitmap's `words`
    // words and the pool after it.
    unsafe { bitmap_ptr.write_bytes(0, (pages * frames::FRAME / 8) as usize) };
    // SAFETY: as above; the words were just zeroed, and the bitmap frames are reserved
    // below, so the allocator never hands them out. The slice lives for the rest of the
    // kernel's run, and nothing else refers to these frames.
    let bitmap = unsafe { slice::from_raw_parts_mut(bitmap_ptr, words as usize) };
    let pool_base = home.base + bitmap_pages * frames::FRAME;
    // SAFETY: the pool's `TABLE_POOL_FRAMES` frames follow the bitmap inside `home`, were
    // zeroed above and are reserved below, so only the pool uses them. `Table` is 4 KiB of
    // `AtomicU64`, valid for any bits and aligned (the frames are 4 KiB aligned); accesses
    // through the shared slice are atomic, so the processor's own accessed/dirty updates
    // do not race with them.
    let pool = unsafe {
        slice::from_raw_parts(
            ptr::with_exposed_provenance::<Table>(phys::to_virt(pool_base) as usize),
            TABLE_POOL_FRAMES as usize,
        )
    };
    // The AP trampoline's code and data frames (M3) are never handed out.
    let trampoline = (smp::TRAMPOLINE, 2);
    let alloc = match FrameAllocator::new(bitmap, regions, &[(home.base, pages), trampoline]) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(com1, "origin-kernel: frame allocator: {e:?}");
            return None;
        }
    };
    let _ = writeln!(
        com1,
        "origin-kernel: frame allocator: {} free frames ({} MiB), bitmap of {} pages at {:#x}",
        alloc.free_frames(),
        (alloc.free_frames() * frames::FRAME) >> 20,
        bitmap_pages,
        home.base
    );
    debug_assert_eq!(PAGE, frames::FRAME);
    Some((alloc, pool, pool_base))
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

/// Sends this processor an NMI (x2APIC ICR, NMI delivery, "self" shorthand) and checks
/// that it was taken on the dedicated NMI stack (IST2), not on the interrupted stack.
fn prove_nmi_ist(com1: &mut Console) -> bool {
    let (before, _) = cpu::nmis();
    // SAFETY: CPL 0, boot processor; x2APIC mode is on (timer bring-up), so this is a no-op
    // transition yielding the handle.
    let lapic = unsafe { timer::X2Apic::enable() };
    // SAFETY: an NMI to this processor runs the NMI handler, which only records it.
    unsafe { lapic.send_ipi(0, (1 << 18) | 0x400) };
    let start = cpu::rdtsc();
    while cpu::nmis().0 == before && cpu::rdtsc().wrapping_sub(start) < 100_000_000 {
        core::hint::spin_loop();
    }
    let (count, frame) = cpu::nmis();
    let (low, high) = cpu::bsp_nmi_stack();
    let on_ist = count > before && (low..high).contains(&frame);
    let _ = writeln!(
        com1,
        "origin-kernel: NMI taken on its own stack (IST2): frame at {frame:#x}, stack {low:#x}..{high:#x}{}",
        if on_ist { "" } else { ": NMI stack failed" }
    );
    on_ist
}

/// Where the SMEP/SMAP probe maps its temporary user page: the top of the lower half.
const USER_PROBE_PAGE: u64 = 0x0000_7FFF_FFFF_E000;

/// Enables SMEP and SMAP and proves both: a temporary user page holding `mov rax, -1;
/// ret`, which a supervisor fetch (SMEP) and a supervisor read (SMAP) must both fault on.
/// The page is removed afterwards.
fn prove_user_isolation(com1: &mut Console) -> bool {
    use alloc::alloc::{Layout, alloc_zeroed, dealloc};
    // SAFETY: CPL 0, and the kernel has no user mappings yet.
    let (smep, smap) = unsafe { cpu::enable_smep_smap() };
    if !(smep && smap) {
        let _ = writeln!(
            com1,
            "origin-kernel: SMEP {smep}, SMAP {smap}: supervisor isolation from user pages incomplete"
        );
        return false;
    }
    let Ok(layout) = Layout::from_size_align(4096, 4096) else {
        return false;
    };
    // SAFETY: `layout` has a non-zero size.
    let frame = unsafe { alloc_zeroed(layout) };
    if frame.is_null() {
        return false;
    }
    // SAFETY: `frame` is a fresh, writable 4 KiB allocation.
    unsafe {
        ptr::copy_nonoverlapping(cpu::PROBE_RETURN.as_ptr(), frame, cpu::PROBE_RETURN.len());
    }
    let phys = phys::to_phys(frame.expose_provenance() as u64);
    let mut ok = true;
    if vm::map_user(
        USER_PROBE_PAGE,
        phys,
        framekernel::paging::Access::ReadExecute,
    )
    .is_err()
    {
        let _ = writeln!(com1, "origin-kernel: cannot map the user probe page");
        ok = false;
    } else {
        // SAFETY: the user page holds `PROBE_RETURN`.
        let fetch = unsafe { cpu::probe_execute(USER_PROBE_PAGE) };
        // SAFETY: the user page is RAM, not MMIO.
        let read = unsafe { cpu::probe_read(USER_PROBE_PAGE) };
        for (what, probe, expected) in [
            ("SMEP: supervisor fetch from a user page", fetch, 0x11),
            ("SMAP: supervisor read of a user page", read, 0x1),
        ] {
            match probe {
                cpu::Probe::Faulted { error, address }
                    if error == expected && address == USER_PROBE_PAGE =>
                {
                    let _ = writeln!(
                        com1,
                        "origin-kernel: page protection: {what} at {USER_PROBE_PAGE:#x} faulted (#PF error {error:#x})"
                    );
                }
                other => {
                    ok = false;
                    let _ = writeln!(
                        com1,
                        "origin-kernel: page protection violated: {what}: {other:?}"
                    );
                }
            }
        }
        if vm::unmap(USER_PROBE_PAGE).is_err() {
            ok = false;
        }
    }
    // SAFETY: `frame` came from `alloc_zeroed(layout)` and is no longer mapped as a user
    // page.
    unsafe { dealloc(frame, layout) };
    ok
}

/// Proves the allocator by allocating, writing, reading back and freeing three frames.
fn prove_frames(com1: &mut Console, alloc: &mut FrameAllocator<'_>) {
    let mut got = [0u64; 3];
    for (i, slot) in got.iter_mut().enumerate() {
        let Some(addr) = alloc.allocate() else {
            let _ = writeln!(com1, "origin-kernel: frame allocator exhausted");
            return;
        };
        let p = ptr::with_exposed_provenance_mut::<u64>(phys::to_virt(addr) as usize);
        let pattern = 0x4F52_4947_494E_0000 | i as u64;
        // SAFETY: the allocator just handed out this usable frame, which the kernel's
        // direct map covers read/write, and nothing else uses it; a volatile write and read
        // prove the RAM really holds it.
        unsafe { p.write_volatile(pattern) };
        // SAFETY: as above.
        let back = unsafe { p.read_volatile() };
        if back != pattern {
            let _ = writeln!(
                com1,
                "origin-kernel: frame {addr:#x} did not keep its contents"
            );
            return;
        }
        *slot = addr;
    }
    for &addr in &got {
        if alloc.free(addr).is_err() {
            let _ = writeln!(com1, "origin-kernel: frame {addr:#x} could not be freed");
            return;
        }
    }
    let _ = writeln!(
        com1,
        "origin-kernel: allocated, wrote and freed 3 frames at {:#x}, {:#x}, {:#x}",
        got[0], got[1], got[2]
    );
}

/// What [`read_boot_info`] returns: the copied header, the region and kernel segment
/// arrays, and the memory summary.
type BootData = (
    BootInfo,
    &'static [MemoryRegion],
    &'static [KernelSegment],
    MemorySummary,
);

/// Copies and validates the boot info, then checks its kernel segment list and summarizes
/// its region array.
///
/// # Safety
///
/// `boot_info` must be null or the address of a readable `BootInfo` whose region and
/// segment arrays, if the header is valid, are readable and are never written afterwards:
/// the returned slices are `'static`, which holds because the loader leaves that memory to
/// the kernel for good.
unsafe fn read_boot_info(boot_info: *const BootInfo) -> Result<BootData, InvalidBootInfo> {
    boot::check_address(boot_info.addr())?;
    // SAFETY: `boot_info` is non-null and aligned (checked above) and readable (the
    // caller's contract). `BootInfo` has only integer fields, so every bit pattern is a
    // valid value. The header is copied, so no reference into loader memory is kept.
    let info = unsafe { boot_info.read() };
    let array = boot::validate(&info)?;
    // Boot info 0.3: the arrays are given by physical address; read them through the direct
    // map the loader built.
    phys::init(&info);
    let first =
        ptr::with_exposed_provenance::<MemoryRegion>(phys::to_virt(array.addr as u64) as usize);
    // SAFETY: `validate` checked that the array is non-null, aligned, at most `isize::MAX`
    // bytes long and does not wrap. The caller's contract makes it readable and unchanging
    // while the slice lives. `MemoryRegion` has only integer fields, so every bit pattern
    // is a valid value.
    let regions = unsafe { slice::from_raw_parts(first, array.len) };
    let memory = boot::summarize(regions)?;
    let segment_array = boot::segment_array(&info)?;
    let first = ptr::with_exposed_provenance::<KernelSegment>(phys::to_virt(
        segment_array.addr as u64,
    ) as usize);
    // SAFETY: as for the regions: `segment_array` checked the layout requirements, the
    // caller's contract makes the memory readable and unchanging, and `KernelSegment` has
    // only integer fields.
    let segments = unsafe { slice::from_raw_parts(first, segment_array.len) };
    boot::check_kernel_segments(segments, &info)?;
    Ok((info, regions, segments, memory))
}

/// Whether the loader applied the image's relocations.
///
/// The compiler knows the link-time value of [`RELOCATION_PROBE`], so the load must be
/// volatile to observe what is really in memory. The address of [`RELOCATION_TARGET`] is
/// computed RIP-relative and is correct wherever the image was loaded.
fn relocations_applied() -> bool {
    let probe = (&raw const RELOCATION_PROBE).cast::<*const u8>();
    // SAFETY: `probe` points to a live, aligned static of pointer size. It is read as a
    // raw pointer, which is valid for any bit pattern, so an unrelocated value is never
    // treated as a reference.
    let stored = unsafe { ptr::read_volatile(probe) };
    ptr::eq(stored, RELOCATION_TARGET.as_ptr())
}

/// Disables interrupts and halts this processor forever.
pub(crate) fn halt() -> ! {
    loop {
        // SAFETY: the kernel runs at CPL 0 (entry contract). `cli; hlt` touches no memory
        // and needs no stack. An NMI or SMI can still resume the processor, so it halts
        // again.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

/// COM1's registers, reached through x86 port I/O.
pub(crate) struct Com1(());

impl Com1 {
    /// # Safety
    ///
    /// The caller must run at CPL 0, and no other processor or driver may program COM1
    /// while the value is used. Accesses through it reach only COM1's registers
    /// 0x3F8–0x3FD, which cannot affect memory.
    pub(crate) unsafe fn new() -> Self {
        Self(())
    }
}

impl UartRegisters for Com1 {
    fn read(&mut self, reg: Register) -> u8 {
        // SAFETY: `Com1::new`'s contract (CPL 0, COM1 owned by the kernel), and the port is
        // one of COM1's registers, so the read cannot affect memory.
        unsafe { inb(COM1_BASE + reg.offset()) }
    }

    fn write(&mut self, reg: Register, value: u8) {
        // SAFETY: as for `read`.
        unsafe { outb(COM1_BASE + reg.offset(), value) }
    }
}

/// Writes `value` to I/O port `port`.
///
/// # Safety
///
/// The processor must be at CPL 0 (or hold I/O permission for `port`), and `port` must be a
/// device register that this code owns and whose write cannot affect memory safety, such as
/// a UART register.
pub(crate) unsafe fn outb(port: u16, value: u8) {
    // SAFETY: the caller's contract. `out` reads no memory, needs no stack and leaves the
    // flags unchanged.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

/// Reads I/O port `port`.
///
/// # Safety
///
/// As for [`outb`]: CPL 0 (or I/O permission), and a register this code owns whose read
/// cannot affect memory safety.
pub(crate) unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: the caller's contract. `in` writes only `al`, reads no memory, needs no stack
    // and leaves the flags unchanged.
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Reads a 32-bit value from I/O port `port`.
///
/// # Safety
///
/// As for [`inb`]: CPL 0 (or I/O permission), and a register this code owns whose read
/// cannot affect memory safety.
pub(crate) unsafe fn ind(port: u16) -> u32 {
    let value: u32;
    // SAFETY: the caller's contract. `in eax, dx` writes only `eax`, reads no memory, needs
    // no stack and leaves the flags unchanged.
    unsafe {
        asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
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
