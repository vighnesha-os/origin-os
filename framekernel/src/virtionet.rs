//! A modern virtio-net driver bring-up (virtio 1.1; CLAUDE.md §31 driver framework, §54
//! networking). It finds the virtio-net PCI function, discovers its capability structures
//! (`virtio::find_caps`), maps the common/notify/device-config regions, runs the status and
//! feature handshake to `DRIVER_OK`, programs MSI-X so the receive queue's used-buffer
//! interrupts reach a chosen processor (`cpu::NET_VECTOR`), sets up the receive and transmit
//! virtqueues with DMA rings (`virtqueue`), and reads the device MAC. A receive thread on that
//! processor then drives a live datapath over the link — it posts receive buffers, resolves
//! QEMU's gateway by ARP, and obtains a DHCP lease (transmit and receive of real
//! Ethernet/IPv4/UDP frames) — and receive is interrupt-driven: for every frame the thread
//! sleeps on a wait queue that only the NIC's interrupt handler wakes.
//!
//! The ring memory is zeroed DMA pages (`storage::dma_page`): the direct map makes them
//! physically contiguous and gives their physical addresses, and x86 DMA is cache-coherent.
//! The `unsafe` here is volatile MMIO and forming slices over those DMA pages.

use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering, fence};

use framekernel::net::{self, Arp, EthFrame, Ipv4, Mac, arp_op, ethertype};
use framekernel::pci::{self, Address};
use framekernel::virtio::{self, feature, status};
use framekernel::virtqueue::{self, Descriptor, F_WRITE};
use origin_sync::SpinLock;

use crate::console::Console;
use crate::wait::WaitQueue;

const VIRTIO_VENDOR: u16 = 0x1AF4;
const Q_RX: u16 = 0;
const Q_TX: u16 = 1;
/// The MAC the boot sets on the device (QEMU `-device virtio-net-pci,mac=…`).
const EXPECTED_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0xAA, 0xBB, 0xCC];
/// The MSI-X table entry the receive queue's interrupts use. Entry 0 is left masked.
const RX_MSIX_ENTRY: u16 = 1;
/// virtio's "no MSI-X vector" sentinel.
const NO_VECTOR: u16 = 0xFFFF;

/// Counts the virtio-net MSI-X interrupts the device has raised.
static NET_IRQS: AtomicU64 = AtomicU64::new(0);
/// The processor the last NIC interrupt arrived on, plus one (0: none yet).
static NET_IRQ_CPU: AtomicU64 = AtomicU64::new(0);
/// The receive thread sleeps here until a used-buffer interrupt wakes it.
static NET_WAIT: WaitQueue = WaitQueue::new();
/// NIC interrupts that found the receive thread asleep and woke it.
static NET_WAKES: AtomicU64 = AtomicU64::new(0);

/// Called from the NIC's interrupt vector (`cpu::NET_VECTOR`): records the used-buffer
/// interrupt and wakes the receive thread, which then reads the used ring itself.
pub(crate) fn on_interrupt() {
    NET_IRQS.fetch_add(1, Ordering::Relaxed);
    NET_IRQ_CPU.store(crate::cpu::cpu_id() + 1, Ordering::Relaxed);
    if NET_WAIT.wake_one() {
        NET_WAKES.fetch_add(1, Ordering::Relaxed);
    }
}

// Common-configuration register offsets (virtio 1.1 §4.1.4.3).
mod reg {
    pub(super) const DEVICE_FEATURE_SELECT: u64 = 0;
    pub(super) const DEVICE_FEATURE: u64 = 4;
    pub(super) const DRIVER_FEATURE_SELECT: u64 = 8;
    pub(super) const DRIVER_FEATURE: u64 = 12;
    pub(super) const CONFIG_MSIX_VECTOR: u64 = 16;
    pub(super) const DEVICE_STATUS: u64 = 20;
    pub(super) const QUEUE_SELECT: u64 = 22;
    pub(super) const QUEUE_SIZE: u64 = 24;
    pub(super) const QUEUE_MSIX_VECTOR: u64 = 26;
    pub(super) const QUEUE_ENABLE: u64 = 28;
    pub(super) const QUEUE_NOTIFY_OFF: u64 = 30;
    pub(super) const QUEUE_DESC: u64 = 32;
    pub(super) const QUEUE_DRIVER: u64 = 40;
    pub(super) const QUEUE_DEVICE: u64 = 48;
}

// --- volatile MMIO over a mapped BAR virtual address ---

fn r8(a: u64) -> u8 {
    // SAFETY: `a` is inside a BAR region mapped uncached by `vm::map_mmio`; the device may
    // change it, so the read is volatile and no reference is formed.
    unsafe { ptr::with_exposed_provenance::<u8>(a as usize).read_volatile() }
}
fn w8(a: u64, v: u8) {
    // SAFETY: as `r8`; a configuration write reaches only this device's register.
    unsafe { ptr::with_exposed_provenance_mut::<u8>(a as usize).write_volatile(v) }
}
fn r16(a: u64) -> u16 {
    // SAFETY: as `r8`, 2-byte access to an aligned register.
    unsafe { ptr::with_exposed_provenance::<u16>(a as usize).read_volatile() }
}
fn w16(a: u64, v: u16) {
    // SAFETY: as `w8`.
    unsafe { ptr::with_exposed_provenance_mut::<u16>(a as usize).write_volatile(v) }
}
fn r32(a: u64) -> u32 {
    // SAFETY: as `r8`, 4-byte access to an aligned register.
    unsafe { ptr::with_exposed_provenance::<u32>(a as usize).read_volatile() }
}
fn w32(a: u64, v: u32) {
    // SAFETY: as `w8`.
    unsafe { ptr::with_exposed_provenance_mut::<u32>(a as usize).write_volatile(v) }
}
fn w64(a: u64, v: u64) {
    w32(a, v as u32);
    w32(a + 4, (v >> 32) as u32);
}

/// A byte-slice view over a DMA page (for the virtqueue helpers to read and write).
fn page(virt: u64, len: usize) -> &'static mut [u8] {
    // SAFETY: `virt` is a live, kept-forever DMA page from `storage::dma_page` of at least
    // one 4 KiB page; `len` is within it. The device accesses it coherently by physical
    // address; this view is used only while no request is outstanding on it.
    unsafe {
        core::slice::from_raw_parts_mut(ptr::with_exposed_provenance_mut::<u8>(virt as usize), len)
    }
}

/// One set-up virtqueue.
#[derive(Clone, Copy)]
struct Queue {
    size: usize,
    desc: u64,
    avail: u64,
    used: u64,
    notify_off: u16,
}

/// What the receive thread needs from the bring-up, handed over before it is spawned.
#[derive(Clone, Copy)]
struct Setup {
    notify_base: u64,
    mul: u32,
    rx: Queue,
    tx: Queue,
    mac: [u8; 6],
}
static SETUP: SpinLock<Option<Setup>> = SpinLock::new(None);

/// Set by the receive thread once its buffers are posted, just before it sleeps for the
/// gateway's ARP reply; the boot flow sends the request only after this.
static RX_ARMED: AtomicU64 = AtomicU64::new(0);
/// The receive thread's results. `RX_DONE` is stored last (Release), after the others.
static RX_DONE: AtomicU64 = AtomicU64::new(0);
/// The gateway's MAC, packed big-endian into the low 48 bits (0: unresolved).
static RX_GATEWAY: AtomicU64 = AtomicU64::new(0);
/// The leased address in the low 32 bits, with bit 32 set once a lease is bound (0: none).
static RX_LEASE: AtomicU64 = AtomicU64::new(0);
/// How often the receive thread blocked waiting for a frame.
static RX_BLOCKED: AtomicU64 = AtomicU64::new(0);

/// Maps the BAR holding capability `loc` and returns the virtual address of its structure.
fn map_structure(dev: &crate::pcie::Device, loc: virtio::CfgLocation) -> Option<u64> {
    let bar = dev.bars.get(loc.bar as usize)?.as_ref()?;
    let base = crate::vm::map_mmio(bar.address, bar.size).ok()?;
    Some(base + u64::from(loc.offset))
}

/// Sets up queue `q`: a smaller-of device size and 256 ring in three DMA pages, with the
/// physical addresses programmed, its MSI-X `vector` assigned (`NO_VECTOR` for none), and
/// the queue enabled. MSI-X must already be enabled on the function for a real vector to
/// take.
fn setup_queue(common: u64, q: u16, vector: u16) -> Option<Queue> {
    w16(common + reg::QUEUE_SELECT, q);
    let dev_size = r16(common + reg::QUEUE_SIZE);
    if dev_size == 0 {
        return None; // queue not available
    }
    let size = dev_size.min(256) as usize;
    w16(common + reg::QUEUE_SIZE, size as u16);
    let notify_off = r16(common + reg::QUEUE_NOTIFY_OFF);
    let (dv, dp) = crate::storage::dma_page()?;
    let (av, ap) = crate::storage::dma_page()?;
    let (uv, up) = crate::storage::dma_page()?;
    w64(common + reg::QUEUE_DESC, dp);
    w64(common + reg::QUEUE_DRIVER, ap);
    w64(common + reg::QUEUE_DEVICE, up);
    w16(common + reg::QUEUE_MSIX_VECTOR, vector);
    w16(common + reg::QUEUE_ENABLE, 1);
    Some(Queue {
        size,
        desc: dv,
        avail: av,
        used: uv,
        notify_off,
    })
}

/// Programs the MSI-X table so entry [`RX_MSIX_ENTRY`] targets processor `target` (an
/// x2APIC ID) at `cpu::NET_VECTOR` (every other entry masked), and turns MSI-X on for the
/// function. Returns whether MSI-X is enabled and unmasked. Mirrors the NVMe driver's setup.
fn enable_msix_table(dev: &crate::pcie::Device, target: u32) -> bool {
    let Some(msix) = crate::pcie::msix(dev.function.at) else {
        return false;
    };
    if msix.entries <= RX_MSIX_ENTRY {
        return false;
    }
    let Some(bar) = dev.bars.get(usize::from(msix.table_bar)).copied().flatten() else {
        return false;
    };
    let table_len = u64::from(msix.entries) * pci::MSIX_ENTRY;
    if u64::from(msix.table_offset) + table_len > bar.size {
        return false;
    }
    let Ok(table) = crate::vm::map_mmio(bar.address + u64::from(msix.table_offset), table_len)
    else {
        return false;
    };
    let (Some(address), Some(data)) = (
        pci::msi_address(target),
        pci::msi_data(crate::cpu::NET_VECTOR),
    ) else {
        return false;
    };
    let entry = |i: u64, word: u64, value: u32| {
        let p = ptr::with_exposed_provenance_mut::<u32>(
            (table + i * pci::MSIX_ENTRY + 4 * word) as usize,
        );
        // SAFETY: inside the MSI-X table (checked against its BAR above), mapped uncached;
        // the entries only direct where the device's interrupt messages go.
        unsafe { p.write_volatile(value) };
    };
    for i in 0..u64::from(msix.entries) {
        entry(i, 3, pci::MSIX_MASKED);
    }
    entry(u64::from(RX_MSIX_ENTRY), 0, address as u32);
    entry(u64::from(RX_MSIX_ENTRY), 1, (address >> 32) as u32);
    entry(u64::from(RX_MSIX_ENTRY), 2, data);
    entry(u64::from(RX_MSIX_ENTRY), 3, 0); // unmask
    let Some(control) = crate::pcie::enable_msix(dev.function.at, &msix) else {
        return false;
    };
    control & pci::MSIX_ENABLE != 0 && control & pci::MSIX_FUNCTION_MASK == 0
}

/// Writes the notify address for queue `q`'s notify offset.
fn notify(notify_base: u64, off: u16, mul: u32, q: u16) {
    fence(Ordering::SeqCst); // the ring writes must be visible before the notification
    w16(notify_base + u64::from(off) * u64::from(mul), q);
}

/// How many receive buffers to post.
const N_RX: usize = 8;
/// The size of each receive buffer (a virtio-net header plus a full frame).
const RX_BUF: usize = 2048;

/// Posts [`N_RX`] device-writable receive buffers on the RX queue (descriptor `i` → buffer
/// `i`), returning their virtual addresses, then notifies the device.
fn post_rx(rx: &Queue, notify_base: u64, mul: u32) -> Option<[u64; N_RX]> {
    let mut pages = [0u64; N_RX];
    let desc = page(rx.desc, virtqueue::desc_bytes(rx.size));
    let avail = page(rx.avail, virtqueue::avail_bytes(rx.size));
    for (i, slot) in pages.iter_mut().enumerate() {
        let (v, p) = crate::storage::dma_page()?;
        *slot = v;
        Descriptor {
            addr: p,
            len: RX_BUF as u32,
            flags: F_WRITE,
            next: 0,
        }
        .write(desc, i);
        virtqueue::avail_push(avail, rx.size, i as u16);
    }
    notify(notify_base, rx.notify_off, mul, Q_RX);
    Some(pages)
}

/// Transmits `frame` (a complete Ethernet frame) on the TX queue, prepending the virtio-net
/// header, and waits for the device to consume it.
fn transmit(tx: &Queue, notify_base: u64, mul: u32, frame: &[u8]) -> bool {
    let Some((fv, fp)) = crate::storage::dma_page() else {
        return false;
    };
    let hdr = virtio::NetHeader::plain().encode();
    let total = hdr.len() + frame.len();
    if total > 4096 {
        return false;
    }
    let buf = page(fv, 4096);
    buf[..hdr.len()].copy_from_slice(&hdr);
    buf[hdr.len()..total].copy_from_slice(frame);
    Descriptor {
        addr: fp,
        len: total as u32,
        flags: 0,
        next: 0,
    }
    .write(page(tx.desc, virtqueue::desc_bytes(tx.size)), 0);
    virtqueue::avail_push(page(tx.avail, virtqueue::avail_bytes(tx.size)), tx.size, 0);
    notify(notify_base, tx.notify_off, mul, Q_TX);
    for _ in 0..5_000_000u64 {
        if r16(tx.used + 2) != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Sleeps until the next received frame and returns its Ethernet bytes (the virtio-net
/// header stripped), advancing `last_used`. An empty slice means a malformed completion the
/// caller should skip.
///
/// The running kernel thread blocks on [`NET_WAIT`] and only the NIC's used-buffer interrupt
/// (`on_interrupt`) wakes it: there is no deadline, so without the interrupt it never runs
/// again (the boot flow's bounded wait for the thread is what notices). The condition reads
/// the used ring itself, so a frame that arrived before the thread registered is seen at
/// once and no wake-up is lost.
fn recv_frame(rx: &Queue, rx_pages: &[u64; N_RX], last_used: &mut u16) -> &'static [u8] {
    let blocked = NET_WAIT.wait_until(|| r16(rx.used + 2) != *last_used);
    RX_BLOCKED.fetch_add(blocked, Ordering::Relaxed);
    fence(Ordering::SeqCst);
    let slot = (*last_used as usize) % rx.size;
    let (id, len) = virtqueue::used_elem(page(rx.used, virtqueue::used_bytes(rx.size)), slot);
    *last_used = last_used.wrapping_add(1);
    let (id, len) = (id as usize, len as usize);
    if id >= N_RX || !(virtio::NET_HDR_LEN..=RX_BUF).contains(&len) {
        return &[];
    }
    &page(rx_pages[id], RX_BUF)[virtio::NET_HDR_LEN..len]
}

/// Builds a broadcast Ethernet frame into `out` and returns its length.
fn build_frame(out: &mut [u8], our_mac: Mac, ethertype: u16, payload: &[u8]) -> Option<usize> {
    EthFrame::write_header(Mac::BROADCAST, our_mac, ethertype, out)?;
    let end = net::ETH_HEADER + payload.len();
    out.get_mut(net::ETH_HEADER..end)?.copy_from_slice(payload);
    Some(end)
}

/// Transmits a DHCP message (broadcast) by wrapping it in UDP (68 → 67), IPv4 and Ethernet.
fn transmit_dhcp(tx: &Queue, notify_base: u64, mul: u32, our_mac: Mac, dhcp: &[u8]) -> bool {
    let any = Ipv4([0, 0, 0, 0]);
    let bcast = Ipv4([255, 255, 255, 255]);
    let mut udp = [0u8; 640];
    let Some(un) = net::Udp::write(any, bcast, 68, 67, dhcp, &mut udp) else {
        return false;
    };
    let mut ip = [0u8; 680];
    let Some(ipn) =
        net::Ipv4Packet::write(any, bcast, net::protocol::UDP, 64, 0, &udp[..un], &mut ip)
    else {
        return false;
    };
    let mut frame = [0u8; 720];
    let Some(fl) = build_frame(&mut frame, our_mac, ethertype::IPV4, &ip[..ipn]) else {
        return false;
    };
    transmit(tx, notify_base, mul, &frame[..fl])
}

/// Extracts the DHCP message (the UDP payload) from a received Ethernet frame addressed to
/// the client port (68), or `None` if it is not such a datagram. The UDP checksum is not
/// re-verified here (the IPv4 header checksum is, by `parse`).
fn dhcp_payload(frame: &[u8]) -> Option<&[u8]> {
    let eth = EthFrame::parse(frame)?;
    if eth.ethertype != ethertype::IPV4 {
        return None;
    }
    let ip = net::Ipv4Packet::parse(eth.payload)?;
    if ip.protocol != net::protocol::UDP || ip.payload.len() < net::UDP_HEADER {
        return None;
    }
    let dst_port = u16::from_be_bytes([ip.payload[2], ip.payload[3]]);
    (dst_port == 68).then(|| &ip.payload[net::UDP_HEADER..])
}

/// QEMU's user-networking gateway, the address the datapath resolves by ARP.
const GATEWAY: Ipv4 = Ipv4([10, 0, 2, 2]);

/// Transmits an ARP who-has for [`GATEWAY`] (a broadcast Ethernet frame) on the TX queue.
fn send_arp_request(tx: &Queue, notify_base: u64, mul: u32, our_mac: Mac) -> bool {
    let mut req = [0u8; net::ARP_LEN];
    let request = Arp {
        operation: arp_op::REQUEST,
        sender_mac: our_mac,
        sender_ip: Ipv4([10, 0, 2, 15]),
        target_mac: Mac([0; 6]),
        target_ip: GATEWAY,
    };
    if request.write(&mut req).is_none() {
        return false;
    }
    let mut frame = [0u8; net::ETH_HEADER + net::ARP_LEN];
    let Some(fl) = build_frame(&mut frame, our_mac, ethertype::ARP, &req) else {
        return false;
    };
    transmit(tx, notify_base, mul, &frame[..fl])
}

/// Sleeps for received frames until [`GATEWAY`]'s ARP reply arrives (at most four frames),
/// returning the gateway's MAC.
fn await_arp_reply(rx: &Queue, rx_pages: &[u64; N_RX], last_used: &mut u16) -> Option<Mac> {
    for _ in 0..4 {
        let eth = recv_frame(rx, rx_pages, last_used);
        if let Some(parsed) = EthFrame::parse(eth)
            && parsed.ethertype == ethertype::ARP
            && let Some(arp) = Arp::parse(parsed.payload)
            && arp.operation == arp_op::REPLY
            && arp.sender_ip == GATEWAY
        {
            return Some(arp.sender_mac);
        }
    }
    None
}

/// Obtains a DHCP lease over the NIC: DISCOVER → OFFER → REQUEST → ACK with QEMU's built-in
/// server, returning the leased address.
fn dhcp_lease(
    rx: &Queue,
    tx: &Queue,
    rx_pages: &[u64; N_RX],
    last_used: &mut u16,
    notify_base: u64,
    mul: u32,
    our_mac: Mac,
) -> Option<Ipv4> {
    let mut client = framekernel::dhcp::Client::new(our_mac.0);
    transmit_dhcp(tx, notify_base, mul, our_mac, &client.start(0x4F52_4947));
    for _ in 0..8 {
        let eth = recv_frame(rx, rx_pages, last_used);
        let Some(message) = dhcp_payload(eth) else {
            continue;
        };
        match client.on_receive(message) {
            framekernel::dhcp::Action::Send(request) => {
                transmit_dhcp(tx, notify_base, mul, our_mac, &request);
            }
            framekernel::dhcp::Action::Bound(lease) => return Some(lease.address),
            _ => {}
        }
    }
    None
}

/// The receive thread: runs the live datapath over the queues the bring-up handed over,
/// sleeping on [`NET_WAIT`] for every frame until the NIC's used-buffer interrupt wakes it.
/// It posts its receive buffers, says so (`RX_ARMED`), and falls asleep waiting for the
/// gateway's ARP reply — the boot flow sends the request only then, so that reply's
/// interrupt is what wakes it. Then it obtains a DHCP lease. Records what it found
/// (`RX_DONE` last), then exits.
extern "sysv64" fn net_thread(_: u64) -> ! {
    let setup = crate::cpu::without_interrupts(|| *SETUP.lock());
    if let Some(s) = setup
        && let Some(rx_pages) = post_rx(&s.rx, s.notify_base, s.mul)
    {
        let mut last_used = 0u16;
        RX_ARMED.store(1, Ordering::Release);
        if let Some(gateway) = await_arp_reply(&s.rx, &rx_pages, &mut last_used) {
            let packed = gateway.0.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b));
            RX_GATEWAY.store(packed, Ordering::Relaxed);
        }
        if let Some(lease) = dhcp_lease(
            &s.rx,
            &s.tx,
            &rx_pages,
            &mut last_used,
            s.notify_base,
            s.mul,
            Mac(s.mac),
        ) {
            let address = u64::from(u32::from_be_bytes(lease.0));
            RX_LEASE.store((1 << 32) | address, Ordering::Relaxed);
        }
    }
    RX_DONE.store(1, Ordering::Release);
    crate::sched::exit()
}

/// Brings up the virtio-net device, hands its queues to a receive thread on another
/// processor (the boot one when there is no other), and reports the result (CLAUDE.md §54).
pub(crate) fn prove(com1: &mut Console, cpus: &[u32], tsc_hz: u64) {
    let Some(dev) = crate::pcie::find(0x02, 0x00, 0x00) else {
        let _ = writeln!(
            com1,
            "origin-kernel: virtio-net: no network device (none to bring up)"
        );
        return;
    };
    if dev.function.vendor != VIRTIO_VENDOR {
        let _ = writeln!(
            com1,
            "origin-kernel: virtio-net: network device {:04x}:{:04x} is not virtio",
            dev.function.vendor, dev.function.device
        );
        return;
    }
    let at: Address = dev.function.at;
    // Enable memory-space decoding and bus mastering for DMA.
    crate::pcie::enable(at, (1 << 1) | (1 << 2));

    let caps = virtio::find_caps(|off| crate::pcie::read_config32(at, off).unwrap_or(0));
    let (Some(common_loc), Some(notify_loc), Some(dev_loc)) =
        (caps.common, caps.notify, caps.device)
    else {
        let _ = writeln!(
            com1,
            "origin-kernel: virtio-net: modern capabilities missing: virtio-net failed"
        );
        return;
    };
    let (Some(common), Some(notify_base), Some(devcfg)) = (
        map_structure(&dev, common_loc),
        map_structure(&dev, notify_loc),
        map_structure(&dev, dev_loc),
    ) else {
        let _ = writeln!(
            com1,
            "origin-kernel: virtio-net: cannot map BARs: virtio-net failed"
        );
        return;
    };

    // Status handshake: reset, acknowledge, driver.
    w8(common + reg::DEVICE_STATUS, 0);
    let mut st = status::ACKNOWLEDGE;
    w8(common + reg::DEVICE_STATUS, st);
    st |= status::DRIVER;
    w8(common + reg::DEVICE_STATUS, st);

    // Read the device features (two 32-bit banks) and negotiate.
    w32(common + reg::DEVICE_FEATURE_SELECT, 0);
    let flo = r32(common + reg::DEVICE_FEATURE);
    w32(common + reg::DEVICE_FEATURE_SELECT, 1);
    let fhi = r32(common + reg::DEVICE_FEATURE);
    let device_features = u64::from(fhi) << 32 | u64::from(flo);
    let wanted = feature::VERSION_1 | feature::NET_MAC | feature::NET_STATUS;
    let Some(accepted) = virtio::negotiate(device_features, wanted) else {
        let _ = writeln!(
            com1,
            "origin-kernel: virtio-net: no VIRTIO_F_VERSION_1: virtio-net failed"
        );
        return;
    };
    w32(common + reg::DRIVER_FEATURE_SELECT, 0);
    w32(common + reg::DRIVER_FEATURE, accepted as u32);
    w32(common + reg::DRIVER_FEATURE_SELECT, 1);
    w32(common + reg::DRIVER_FEATURE, (accepted >> 32) as u32);

    st |= status::FEATURES_OK;
    w8(common + reg::DEVICE_STATUS, st);
    let features_ok = r8(common + reg::DEVICE_STATUS) & status::FEATURES_OK != 0;

    // The receive thread runs on another processor when there is one (as the NVMe interrupt
    // proof does), and the NIC's MSI-X message is routed to that processor, so it idles until
    // the interrupt wakes it. Turn MSI-X on (so the queues can take vectors), with no
    // config-change interrupt, then set up the receive queue (interrupt on entry
    // RX_MSIX_ENTRY) and the transmit queue (no interrupt; its completion is polled).
    let bsp = crate::cpu::cpu_id();
    let target = cpus
        .iter()
        .copied()
        .find(|&id| u64::from(id) != bsp)
        .unwrap_or(bsp as u32);
    let msix_enabled = enable_msix_table(&dev, target);
    w16(common + reg::CONFIG_MSIX_VECTOR, NO_VECTOR);
    let rx = setup_queue(common, Q_RX, RX_MSIX_ENTRY);
    let tx = setup_queue(common, Q_TX, NO_VECTOR);
    let queues_ok = rx.is_some() && tx.is_some();
    // Confirm the receive queue kept its vector (NO_VECTOR back means none was available).
    w16(common + reg::QUEUE_SELECT, Q_RX);
    let rx_vector_ok = msix_enabled && r16(common + reg::QUEUE_MSIX_VECTOR) == RX_MSIX_ENTRY;

    st |= status::DRIVER_OK;
    w8(common + reg::DEVICE_STATUS, st);
    let driver_ok = r8(common + reg::DEVICE_STATUS) & status::DRIVER_OK != 0;

    // Read the MAC and the link status from the device config.
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = r8(devcfg + i as u64);
    }
    let status_word = r16(devcfg + 6);
    let link_up = status_word & 1 != 0;

    // Live datapath over the NIC, interrupt-driven: hand the queues to a receive thread on
    // `target`. Once it has posted its buffers (`RX_ARMED`), give it 10 ms to fall asleep on
    // the NIC's interrupt (it needs microseconds), then send the gateway an ARP request from
    // here, so the reply's interrupt is what wakes the sleeping thread. It then obtains a DHCP
    // lease itself. The boot flow waits for it, bounded, so a NIC whose interrupt never wakes
    // the thread fails here instead of hanging.
    let mul = caps.notify_off_multiplier;
    let (mut spawned, mut arp_sent) = (false, false);
    if let (Some(rxq), Some(txq)) = (rx, tx) {
        crate::cpu::without_interrupts(|| {
            *SETUP.lock() = Some(Setup {
                notify_base,
                mul,
                rx: rxq,
                tx: txq,
                mac,
            });
        });
        spawned = crate::sched::spawn(target as usize, net_thread, 0).is_ok();
        if spawned {
            let armed_or_done =
                || RX_ARMED.load(Ordering::Acquire) != 0 || RX_DONE.load(Ordering::Acquire) != 0;
            crate::sched::yield_until(armed_or_done, tsc_hz, 10);
            let settle = crate::cpu::rdtsc();
            crate::sched::yield_until(
                || crate::cpu::rdtsc().wrapping_sub(settle) > tsc_hz / 100,
                tsc_hz,
                1,
            );
            if RX_ARMED.load(Ordering::Acquire) != 0 {
                arp_sent = send_arp_request(&txq, notify_base, mul, Mac(mac));
            }
            crate::sched::yield_until(|| RX_DONE.load(Ordering::Acquire) != 0, tsc_hz, 10);
        }
    }
    let finished = RX_DONE.load(Ordering::Acquire) != 0;
    let gateway = RX_GATEWAY.load(Ordering::Relaxed);
    let lease = RX_LEASE.load(Ordering::Relaxed);
    let blocked = RX_BLOCKED.load(Ordering::Relaxed);
    let wakes = NET_WAKES.load(Ordering::Relaxed);
    let irqs = NET_IRQS.load(Ordering::Relaxed);
    let on = NET_IRQ_CPU.load(Ordering::Relaxed).saturating_sub(1);
    let last = if irqs == 0 {
        alloc::string::String::from("none arrived")
    } else {
        alloc::format!("the last on CPU {on}")
    };

    let mac_ok = mac == EXPECTED_MAC;
    let consistent = features_ok
        && queues_ok
        && driver_ok
        && mac_ok
        && link_up
        && rx_vector_ok
        && spawned
        && arp_sent
        && finished
        && gateway != 0
        && lease & (1 << 32) != 0
        && blocked > 0
        && wakes > 0
        && irqs > 0
        && on == u64::from(target);
    let g = gateway.to_be_bytes();
    let ip = (lease as u32).to_be_bytes();
    let _ = writeln!(
        com1,
        "origin-kernel: virtio-net: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, features_ok {features_ok}, link_up {link_up}, DRIVER_OK {driver_ok}; ARP 10.0.2.2 -> {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}; DHCP lease {}.{}.{}.{}; receive thread on CPU {target}: slept {blocked}, woken by the NIC's interrupt {wakes} (of {irqs} MSI-X interrupts, {last}); consistent {}{}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        g[2],
        g[3],
        g[4],
        g[5],
        g[6],
        g[7],
        ip[0],
        ip[1],
        ip[2],
        ip[3],
        consistent,
        if consistent {
            ""
        } else {
            ": virtio-net failed"
        }
    );
}
