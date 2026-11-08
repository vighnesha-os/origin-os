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
/// The MSI-X table entry the transmit queue's completion interrupts use.
const TX_MSIX_ENTRY: u16 = 2;
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

/// What the receive thread needs from the bring-up, handed over before it is spawned. (It
/// transmits through [`TX`].)
#[derive(Clone, Copy)]
struct Setup {
    notify_base: u64,
    mul: u32,
    rx: Queue,
    mac: [u8; 6],
}
static SETUP: SpinLock<Option<Setup>> = SpinLock::new(None);

/// Transmit descriptors in use. Each has one buffer page allocated at bring-up and kept: a
/// small pool, recycled as the device completes frames.
const N_TX: usize = 4;
/// Gratuitous ARP announcements of the leased address the receive thread sends as one
/// batch: one more than the ring holds, so the batch fills it and the last frame must wait
/// for a completion. With the ARP request, DISCOVER and REQUEST: eight frames through four
/// descriptors.
const ANNOUNCEMENTS: usize = N_TX + 1;

/// The transmit ring: descriptors `0..N_TX`, each with a fixed buffer, recycled through a
/// free list as the device reports them completed in the used ring.
struct TxRing {
    q: Queue,
    notify_base: u64,
    mul: u32,
    /// Each descriptor's buffer: (virtual, physical).
    bufs: [(u64, u64); N_TX],
    free: virtqueue::FreeList,
    in_flight: [bool; N_TX],
    /// The used ring is consumed up to here.
    last_used: u16,
    sent: u64,
    reclaimed: u64,
}
/// The NIC's transmit ring, shared by the boot flow and the receive thread. Locked with
/// interrupts disabled, never across a wait; the TX-completion handler does not take it.
static TX: SpinLock<Option<TxRing>> = SpinLock::new(None);
/// A transmitter sleeps here (for a free descriptor, or to flush) until a TX-completion
/// interrupt wakes it. Separate from [`NET_WAIT`], so a TX completion never wakes the
/// receive thread in place of the receive interrupt.
static TX_WAIT: WaitQueue = WaitQueue::new();
/// Counts the NIC's TX-completion interrupts.
static TX_IRQS: AtomicU64 = AtomicU64::new(0);
/// TX-completion interrupts that found a transmitter asleep and woke it.
static TX_WAKES: AtomicU64 = AtomicU64::new(0);
/// How often a transmit found every descriptor in flight.
static TX_FULL: AtomicU64 = AtomicU64::new(0);
/// How often a transmitter blocked on [`TX_WAIT`].
static TX_SLEPT: AtomicU64 = AtomicU64::new(0);

/// Called from the NIC's TX-completion vector (`cpu::NET_TX_VECTOR`): counts it and wakes
/// a transmitter waiting for a descriptor, which then reclaims the completions itself.
pub(crate) fn on_tx_interrupt() {
    TX_IRQS.fetch_add(1, Ordering::Relaxed);
    if TX_WAIT.wake_one() {
        TX_WAKES.fetch_add(1, Ordering::Relaxed);
    }
}

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

/// Programs the MSI-X table so entries [`RX_MSIX_ENTRY`] and [`TX_MSIX_ENTRY`] target
/// processor `target` (an x2APIC ID) at `cpu::NET_VECTOR` and `cpu::NET_TX_VECTOR` (every
/// other entry masked), and turns MSI-X on for the function. Returns whether MSI-X is enabled
/// and unmasked. Mirrors the NVMe driver's setup.
fn enable_msix_table(dev: &crate::pcie::Device, target: u32) -> bool {
    let Some(msix) = crate::pcie::msix(dev.function.at) else {
        return false;
    };
    if msix.entries <= RX_MSIX_ENTRY.max(TX_MSIX_ENTRY) {
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
    let (Some(address), Some(rx_data), Some(tx_data)) = (
        pci::msi_address(target),
        pci::msi_data(crate::cpu::NET_VECTOR),
        pci::msi_data(crate::cpu::NET_TX_VECTOR),
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
    // Receive completions on `NET_VECTOR`, transmit completions on `NET_TX_VECTOR`, both to
    // `target`; every other entry stays masked.
    for (index, data) in [(RX_MSIX_ENTRY, rx_data), (TX_MSIX_ENTRY, tx_data)] {
        let i = u64::from(index);
        entry(i, 0, address as u32);
        entry(i, 1, (address >> 32) as u32);
        entry(i, 2, data);
        entry(i, 3, 0); // unmask
    }
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

impl TxRing {
    /// A ring over `q` with one buffer page, allocated now and kept, for each of its first
    /// [`N_TX`] descriptors.
    fn new(q: Queue, notify_base: u64, mul: u32) -> Option<Self> {
        if q.size < N_TX {
            return None;
        }
        let mut bufs = [(0, 0); N_TX];
        for buf in &mut bufs {
            *buf = crate::storage::dma_page()?;
        }
        Some(Self {
            q,
            notify_base,
            mul,
            bufs,
            free: virtqueue::FreeList::new(N_TX),
            in_flight: [false; N_TX],
            last_used: 0,
            sent: 0,
            reclaimed: 0,
        })
    }

    /// Whether the device has completed frames that are not reclaimed yet.
    fn completion_pending(&self) -> bool {
        r16(self.q.used + 2) != self.last_used
    }

    /// Returns every completed frame's descriptor to the free list. The device reports the
    /// ids: one out of range or not in flight is consumed but never freed (so never twice).
    fn reclaim(&mut self) {
        while self.completion_pending() {
            fence(Ordering::SeqCst);
            let slot = usize::from(self.last_used) % self.q.size;
            let (id, _) =
                virtqueue::used_elem(page(self.q.used, virtqueue::used_bytes(self.q.size)), slot);
            self.last_used = self.last_used.wrapping_add(1);
            if let Some(busy) = self.in_flight.get_mut(id as usize)
                && *busy
            {
                *busy = false;
                self.free.free(id as u16);
                self.reclaimed += 1;
            }
        }
    }

    /// Tells the device that frames are available (the TX queue's notify register).
    fn kick(&self) {
        notify(self.notify_base, self.q.notify_off, self.mul, Q_TX);
    }

    /// Copies `frame` behind a virtio-net header into a free descriptor's buffer and makes
    /// it available to the device, kicking the device when `kick` (a batch kicks once, at
    /// its end or when the ring is full). `false` when every descriptor is in flight.
    fn enqueue(&mut self, frame: &[u8], kick: bool) -> bool {
        let Some(id) = self.free.alloc() else {
            return false;
        };
        let i = usize::from(id);
        let Some(&(virt, phys)) = self.bufs.get(i) else {
            self.free.free(id); // cannot happen: the free list holds only 0..N_TX
            return false;
        };
        // The descriptor is free, so the device is done with this buffer.
        let hdr = virtio::NetHeader::plain().encode();
        let total = hdr.len() + frame.len();
        let buf = page(virt, 4096);
        buf[..hdr.len()].copy_from_slice(&hdr);
        buf[hdr.len()..total].copy_from_slice(frame);
        Descriptor {
            addr: phys,
            len: total as u32,
            flags: 0,
            next: 0,
        }
        .write(page(self.q.desc, virtqueue::desc_bytes(self.q.size)), i);
        if let Some(busy) = self.in_flight.get_mut(i) {
            *busy = true;
        }
        virtqueue::avail_push(
            page(self.q.avail, virtqueue::avail_bytes(self.q.size)),
            self.q.size,
            id,
        );
        self.sent += 1;
        if kick {
            self.kick();
        }
        true
    }
}

/// The condition a transmitter sleeps on: true once the device has completed frames that
/// are not reclaimed yet; otherwise it kicks the device (so frames queued without a kick
/// get sent) and returns false. It runs inside [`WaitQueue::wait_until`] with interrupts
/// disabled, before the sleeper registers, and the TX-completion interrupt is routed to the
/// sender's processor: so the completion's interrupt can arrive only after the sender is
/// registered, and cannot be missed.
fn tx_kick_then_pending() -> bool {
    TX.lock().as_ref().is_some_and(|ring| {
        if ring.completion_pending() {
            return true;
        }
        ring.kick();
        false
    })
}

/// Sleeps on [`TX_WAIT`] until a TX completion is pending (see [`tx_kick_then_pending`]).
fn tx_sleep_for_completion() {
    let blocked = TX_WAIT.wait_until(tx_kick_then_pending);
    TX_SLEPT.fetch_add(blocked, Ordering::Relaxed);
}

/// Transmits `frame` (a complete Ethernet frame) through the TX ring, kicking the device now
/// when `kick`, or leaving it queued for a later kick (a batch). It reclaims completed
/// descriptors first, and while every descriptor is in flight sleeps on [`TX_WAIT`] until
/// the NIC's TX-completion interrupt reports one done (a kernel thread sleeps; the boot flow
/// yields). Returns whether the frame was queued.
fn transmit_with(frame: &[u8], kick: bool) -> bool {
    if virtio::NET_HDR_LEN + frame.len() > 4096 {
        return false;
    }
    loop {
        let queued = crate::cpu::without_interrupts(|| {
            let mut ring = TX.lock();
            let ring = ring.as_mut()?;
            ring.reclaim();
            Some(ring.enqueue(frame, kick))
        });
        match queued {
            None => return false,
            Some(true) => return true,
            Some(false) => {
                TX_FULL.fetch_add(1, Ordering::Relaxed);
                tx_sleep_for_completion();
            }
        }
    }
}

/// Transmits `frame` and kicks the device at once.
fn transmit(frame: &[u8]) -> bool {
    transmit_with(frame, true)
}

/// Kicks the device and sleeps on [`TX_WAIT`] until every queued frame has completed and
/// its descriptor is back in the pool.
fn tx_flush() {
    loop {
        let idle = crate::cpu::without_interrupts(|| {
            TX.lock().as_mut().is_none_or(|ring| {
                ring.reclaim();
                ring.free.available() == N_TX
            })
        });
        if idle {
            return;
        }
        tx_sleep_for_completion();
    }
}

/// The receive ring: buffers `0..N_RX` (kept DMA pages, posted once by [`post_rx`]) that
/// the device fills, recycled one at a time — the buffer holding the frame [`RxRing::recv`]
/// returned goes back to the device at the next `recv`, so the ring never runs dry.
struct RxRing {
    q: Queue,
    pages: [u64; N_RX],
    notify_base: u64,
    mul: u32,
    /// The used ring is consumed up to here.
    last_used: u16,
    /// Each buffer is the device's to fill (posted) or the driver's (returned, not yet
    /// re-posted): a device-reported id that is not posted is refused, never re-posted twice.
    posted: [bool; N_RX],
    /// The buffer holding the frame the last `recv` returned.
    held: Option<u16>,
    received: u64,
    recycled: u64,
}

impl RxRing {
    /// The ring over `q` whose buffers `post_rx` posted at `pages`.
    fn new(q: Queue, pages: [u64; N_RX], notify_base: u64, mul: u32) -> Self {
        Self {
            q,
            pages,
            notify_base,
            mul,
            last_used: 0,
            posted: [true; N_RX],
            held: None,
            received: 0,
            recycled: 0,
        }
    }

    /// Hands the previous frame's buffer back to the device, then sleeps until the next
    /// received frame and returns its Ethernet bytes (the virtio-net header stripped). The
    /// slice is valid until the next `recv` (the borrow of `self` enforces it). An empty
    /// slice means a malformed completion the caller should skip.
    ///
    /// The running kernel thread blocks on [`NET_WAIT`] and only the NIC's used-buffer
    /// interrupt (`on_interrupt`) wakes it: there is no deadline, so without the interrupt it
    /// never runs again (the boot flow's bounded wait for the thread is what notices). The
    /// condition reads the used ring itself, so a frame that arrived before the thread
    /// registered is seen at once and no wake-up is lost.
    fn recv(&mut self) -> &[u8] {
        if let Some(id) = self.held.take() {
            // Its descriptor still describes the same page, so publishing the id is enough.
            if let Some(posted) = self.posted.get_mut(usize::from(id)) {
                *posted = true;
            }
            virtqueue::avail_push(
                page(self.q.avail, virtqueue::avail_bytes(self.q.size)),
                self.q.size,
                id,
            );
            notify(self.notify_base, self.q.notify_off, self.mul, Q_RX);
            self.recycled += 1;
        }
        let (used, last) = (self.q.used, self.last_used);
        let blocked = NET_WAIT.wait_until(|| r16(used + 2) != last);
        RX_BLOCKED.fetch_add(blocked, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        let slot = usize::from(self.last_used) % self.q.size;
        let (id, len) =
            virtqueue::used_elem(page(self.q.used, virtqueue::used_bytes(self.q.size)), slot);
        self.last_used = self.last_used.wrapping_add(1);
        self.received += 1;
        let (i, len) = (id as usize, len as usize);
        let (Some(posted), Some(&virt)) = (self.posted.get_mut(i), self.pages.get(i)) else {
            return &[]; // an id we never posted: nothing to read or recycle
        };
        if !*posted {
            return &[]; // a buffer the device already returned: refuse it
        }
        *posted = false;
        self.held = Some(id as u16);
        if !(virtio::NET_HDR_LEN..=RX_BUF).contains(&len) {
            return &[];
        }
        &page(virt, RX_BUF)[virtio::NET_HDR_LEN..len]
    }
}

/// Builds a broadcast Ethernet frame into `out` and returns its length.
fn build_frame(
    out: &mut [u8],
    dst: Mac,
    our_mac: Mac,
    ethertype: u16,
    payload: &[u8],
) -> Option<usize> {
    EthFrame::write_header(dst, our_mac, ethertype, out)?;
    let end = net::ETH_HEADER + payload.len();
    out.get_mut(net::ETH_HEADER..end)?.copy_from_slice(payload);
    Some(end)
}

/// Transmits a DHCP message (broadcast) by wrapping it in UDP (68 → 67), IPv4 and Ethernet.
fn transmit_dhcp(our_mac: Mac, dhcp: &[u8]) -> bool {
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
    let Some(fl) = build_frame(
        &mut frame,
        Mac::BROADCAST,
        our_mac,
        ethertype::IPV4,
        &ip[..ipn],
    ) else {
        return false;
    };
    transmit(&frame[..fl])
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

/// Transmits an ARP request (a broadcast Ethernet frame) asking for `target_ip`, from
/// `sender_ip`, kicking the device at once when `kick` (else it joins a batch). With both
/// addresses equal it is a gratuitous announcement (RFC 5227 §2.3).
fn send_arp(our_mac: Mac, sender_ip: Ipv4, target_ip: Ipv4, kick: bool) -> bool {
    let mut req = [0u8; net::ARP_LEN];
    let request = Arp {
        operation: arp_op::REQUEST,
        sender_mac: our_mac,
        sender_ip,
        target_mac: Mac([0; 6]),
        target_ip,
    };
    if request.write(&mut req).is_none() {
        return false;
    }
    let mut frame = [0u8; net::ETH_HEADER + net::ARP_LEN];
    let Some(fl) = build_frame(&mut frame, Mac::BROADCAST, our_mac, ethertype::ARP, &req) else {
        return false;
    };
    transmit_with(&frame[..fl], kick)
}

/// Sleeps for received frames until [`GATEWAY`]'s ARP reply arrives (at most four frames),
/// returning the gateway's MAC.
fn await_arp_reply(rx: &mut RxRing) -> Option<Mac> {
    for _ in 0..4 {
        let eth = rx.recv();
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

/// The identifier our echo requests carry ("OR").
const PING_ID: u16 = 0x4F52;
/// Echo requests the receive thread sends the gateway once it holds a lease.
const PINGS: u16 = 4;

/// Echo replies received that matched their request, and their round trips in TSC cycles.
static PING_REPLIES: AtomicU64 = AtomicU64::new(0);
static PING_MIN: AtomicU64 = AtomicU64::new(u64::MAX);
static PING_MAX: AtomicU64 = AtomicU64::new(0);
static PING_TOTAL: AtomicU64 = AtomicU64::new(0);

/// The data echo request `sequence` carries: 32 bytes that differ per request.
fn ping_data(sequence: u16) -> [u8; 32] {
    let mut data = [0u8; 32];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7) ^ (sequence as u8);
    }
    data
}

/// Transmits an ICMP echo request from `src` to `dst`, unicast to `dst_mac`.
fn send_ping(our_mac: Mac, dst_mac: Mac, src: Ipv4, dst: Ipv4, sequence: u16) -> bool {
    let data = ping_data(sequence);
    let mut icmp = [0u8; net::ICMP_HEADER + 32];
    let Some(il) = net::IcmpEcho::write(
        net::icmp_type::ECHO_REQUEST,
        PING_ID,
        sequence,
        &data,
        &mut icmp,
    ) else {
        return false;
    };
    let mut ip = [0u8; net::IPV4_HEADER + net::ICMP_HEADER + 32];
    let Some(ipl) = net::Ipv4Packet::write(
        src,
        dst,
        net::protocol::ICMP,
        64,
        sequence,
        &icmp[..il],
        &mut ip,
    ) else {
        return false;
    };
    let mut frame = [0u8; net::ETH_HEADER + net::IPV4_HEADER + net::ICMP_HEADER + 32];
    let Some(fl) = build_frame(&mut frame, dst_mac, our_mac, ethertype::IPV4, &ip[..ipl]) else {
        return false;
    };
    transmit(&frame[..fl])
}

/// Sleeps for received frames until the echo reply to request `sequence` arrives from
/// `from` to `to` (at most four frames). The reply must carry our identifier, the sequence
/// and the request's data, with valid IPv4 and ICMP checksums.
fn await_echo_reply(rx: &mut RxRing, from: Ipv4, to: Ipv4, sequence: u16) -> bool {
    let data = ping_data(sequence);
    for _ in 0..4 {
        let eth = rx.recv();
        if let Some(frame) = EthFrame::parse(eth)
            && frame.ethertype == ethertype::IPV4
            && let Some(packet) = net::Ipv4Packet::parse(frame.payload)
            && packet.protocol == net::protocol::ICMP
            && packet.src == from
            && packet.dst == to
            && let Some(echo) = net::IcmpEcho::parse(packet.payload)
            && echo.kind == net::icmp_type::ECHO_REPLY
            && echo.identifier == PING_ID
            && echo.sequence == sequence
            && echo.data == data
        {
            return true;
        }
    }
    false
}

/// Pings the gateway [`PINGS`] times from the leased address `ours`, unicast to the MAC the
/// ARP exchange resolved, sleeping on the receive interrupt for each reply, and records the
/// replies that matched and their round-trip times.
fn ping_gateway(rx: &mut RxRing, our_mac: Mac, gateway_mac: Mac, ours: Ipv4) {
    for sequence in 1..=PINGS {
        let start = crate::cpu::rdtsc();
        if send_ping(our_mac, gateway_mac, ours, GATEWAY, sequence)
            && await_echo_reply(rx, GATEWAY, ours, sequence)
        {
            let rtt = crate::cpu::rdtsc().wrapping_sub(start);
            PING_REPLIES.fetch_add(1, Ordering::Relaxed);
            PING_MIN.fetch_min(rtt, Ordering::Relaxed);
            PING_MAX.fetch_max(rtt, Ordering::Relaxed);
            PING_TOTAL.fetch_add(rtt, Ordering::Relaxed);
        }
    }
}

/// The file fetched over TFTP: `xtask` points slirp's built-in TFTP server (`tftp=`) at
/// `framekernel/assets`, the directory the kernel's own initramfs image comes from.
const TFTP_FILE: &str = "initramfs.tar";
/// Our UDP port for the transfer.
const TFTP_CLIENT_PORT: u16 = 49152;
/// The largest file the fetch accepts.
const TFTP_LIMIT: usize = 64 * 1024;

/// The fetch's results: bytes and blocks accepted, whether it completed and matched the
/// embedded image (1), and the server's error code plus one (0: none).
static TFTP_BYTES: AtomicU64 = AtomicU64::new(0);
static TFTP_BLOCKS: AtomicU64 = AtomicU64::new(0);
static TFTP_COMPLETE: AtomicU64 = AtomicU64::new(0);
static TFTP_IDENTICAL: AtomicU64 = AtomicU64::new(0);
static TFTP_ERROR: AtomicU64 = AtomicU64::new(0);
/// Frames the receive ring delivered and buffers it handed back, as the thread finished.
static RX_FRAMES: AtomicU64 = AtomicU64::new(0);
static RX_RECYCLED: AtomicU64 = AtomicU64::new(0);

/// Transmits a UDP datagram from `src`:`sport` to `dst`:`dport` (with its checksum), unicast
/// to `dst_mac`.
fn send_udp(
    our_mac: Mac,
    dst_mac: Mac,
    (src, sport): (Ipv4, u16),
    (dst, dport): (Ipv4, u16),
    payload: &[u8],
) -> bool {
    let mut udp = [0u8; net::UDP_HEADER + 600];
    let Some(un) = net::Udp::write(src, dst, sport, dport, payload, &mut udp) else {
        return false;
    };
    let mut ip = [0u8; net::IPV4_HEADER + net::UDP_HEADER + 600];
    let Some(ipn) =
        net::Ipv4Packet::write(src, dst, net::protocol::UDP, 64, 0, &udp[..un], &mut ip)
    else {
        return false;
    };
    let mut frame = [0u8; net::ETH_HEADER + net::IPV4_HEADER + net::UDP_HEADER + 600];
    let Some(fl) = build_frame(&mut frame, dst_mac, our_mac, ethertype::IPV4, &ip[..ipn]) else {
        return false;
    };
    transmit(&frame[..fl])
}

/// Sleeps for received frames until a UDP datagram from `from` to `to`:`port` arrives (at
/// most `tries` frames), copies its payload into `out` and returns its source port and
/// length. The IPv4 header checksum and the UDP checksum are verified; a datagram whose
/// payload does not fit `out` is skipped.
fn await_udp(
    rx: &mut RxRing,
    (from, to, port): (Ipv4, Ipv4, u16),
    tries: usize,
    out: &mut [u8],
) -> Option<(u16, usize)> {
    for _ in 0..tries {
        let eth = rx.recv();
        if let Some(frame) = EthFrame::parse(eth)
            && frame.ethertype == ethertype::IPV4
            && let Some(packet) = net::Ipv4Packet::parse(frame.payload)
            && packet.protocol == net::protocol::UDP
            && packet.src == from
            && packet.dst == to
            && let Some(udp) = net::Udp::parse(packet.payload, from, to)
            && udp.dst_port == port
            && let Some(dst) = out.get_mut(..udp.payload.len())
        {
            dst.copy_from_slice(udp.payload);
            return Some((udp.src_port, udp.payload.len()));
        }
    }
    None
}

/// Fetches [`TFTP_FILE`] from the TFTP server on the gateway (`tftp::Download` drives the
/// protocol): a read request to port 69 from the leased address, then each DATA block
/// acknowledged to the port the server answers from, until the short final block — the
/// thread sleeping on the receive interrupt for every block. Records the outcome and
/// whether the file matches the image built into the kernel.
fn fetch_tftp(rx: &mut RxRing, our_mac: Mac, server_mac: Mac, ours: Ipv4) {
    use framekernel::tftp;

    let mut request = [0u8; 64];
    let Some(n) = tftp::read_request(TFTP_FILE, &mut request) else {
        return;
    };
    let ours_port = (ours, TFTP_CLIENT_PORT);
    if !send_udp(
        our_mac,
        server_mac,
        ours_port,
        (GATEWAY, tftp::PORT),
        &request[..n],
    ) {
        return;
    }
    let mut download = tftp::Download::new(TFTP_LIMIT);
    let mut server_port = None;
    let mut packet = [0u8; 4 + tftp::BLOCK];
    // One frame per block, plus room for strays.
    for _ in 0..TFTP_LIMIT / tftp::BLOCK + 8 {
        let Some((port, len)) = await_udp(rx, (GATEWAY, ours, TFTP_CLIENT_PORT), 4, &mut packet)
        else {
            break;
        };
        // The transfer continues with the port the server first answered from (its TID).
        if *server_port.get_or_insert(port) != port {
            continue;
        }
        match download.on_packet(&packet[..len]) {
            tftp::Step::Ack { block, last } => {
                let mut ack = [0u8; 4];
                if let Some(an) = tftp::ack(block, &mut ack) {
                    send_udp(our_mac, server_mac, ours_port, (GATEWAY, port), &ack[..an]);
                }
                if last {
                    break;
                }
            }
            tftp::Step::Failed(_) => break,
            tftp::Step::Ignore => {}
        }
    }
    TFTP_BYTES.store(download.data().len() as u64, Ordering::Relaxed);
    TFTP_BLOCKS.store(u64::from(download.blocks()), Ordering::Relaxed);
    TFTP_COMPLETE.store(u64::from(download.is_complete()), Ordering::Relaxed);
    let identical = download.is_complete() && download.data() == crate::INITRAMFS_IMAGE;
    TFTP_IDENTICAL.store(u64::from(identical), Ordering::Relaxed);
    if let Some(tftp::Failure::Server(code)) = download.failure() {
        TFTP_ERROR.store(u64::from(code) + 1, Ordering::Relaxed);
    }
}

/// Obtains a DHCP lease over the NIC: DISCOVER → OFFER → REQUEST → ACK with QEMU's built-in
/// server, returning the leased address.
fn dhcp_lease(rx: &mut RxRing, our_mac: Mac) -> Option<Ipv4> {
    let mut client = framekernel::dhcp::Client::new(our_mac.0);
    transmit_dhcp(our_mac, &client.start(0x4F52_4947));
    for _ in 0..8 {
        let eth = rx.recv();
        let Some(message) = dhcp_payload(eth) else {
            continue;
        };
        match client.on_receive(message) {
            framekernel::dhcp::Action::Send(request) => {
                transmit_dhcp(our_mac, &request);
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
        let mut rx = RxRing::new(s.rx, rx_pages, s.notify_base, s.mul);
        RX_ARMED.store(1, Ordering::Release);
        let gateway = await_arp_reply(&mut rx);
        if let Some(gateway) = gateway {
            let packed = gateway.0.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b));
            RX_GATEWAY.store(packed, Ordering::Relaxed);
        }
        let lease = dhcp_lease(&mut rx, Mac(s.mac));
        if let Some(lease) = lease {
            let address = u64::from(u32::from_be_bytes(lease.0));
            RX_LEASE.store((1 << 32) | address, Ordering::Relaxed);
            // Announce the bound address with gratuitous ARPs (back to back here, not
            // RFC 5227's two-second spacing; nothing answers them), as one batch queued
            // without kicking the device. The batch is one frame larger than the ring, so it
            // fills the ring and the last frame's sender kicks the device and sleeps until
            // the TX-completion interrupt frees a descriptor, which is then reused.
            for _ in 0..ANNOUNCEMENTS {
                send_arp(Mac(s.mac), Ipv4(lease.0), Ipv4(lease.0), false);
            }
        }
        // Kick whatever the batch left queued, and sleep until every frame (the boot flow's
        // ARP request included) has completed and its descriptor is back in the pool.
        tx_flush();
        // Use the lease: ping the gateway from the leased address, unicast to the MAC the
        // ARP exchange resolved, fetch a file from its TFTP server, then flush those frames.
        if let (Some(gateway), Some(lease)) = (gateway, lease) {
            ping_gateway(&mut rx, Mac(s.mac), gateway, lease);
            fetch_tftp(&mut rx, Mac(s.mac), gateway, lease);
            tx_flush();
        }
        RX_FRAMES.store(rx.received, Ordering::Relaxed);
        RX_RECYCLED.store(rx.recycled, Ordering::Relaxed);
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
    let tx = setup_queue(common, Q_TX, TX_MSIX_ENTRY);
    let queues_ok = rx.is_some() && tx.is_some();
    // Confirm each queue kept its vector (NO_VECTOR back means none was available).
    w16(common + reg::QUEUE_SELECT, Q_RX);
    let rx_vector_ok = msix_enabled && r16(common + reg::QUEUE_MSIX_VECTOR) == RX_MSIX_ENTRY;
    w16(common + reg::QUEUE_SELECT, Q_TX);
    let tx_vector_ok = msix_enabled && r16(common + reg::QUEUE_MSIX_VECTOR) == TX_MSIX_ENTRY;

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
    if let (Some(rxq), Some(txq)) = (rx, tx)
        && let Some(ring) = TxRing::new(txq, notify_base, mul)
    {
        crate::cpu::without_interrupts(|| {
            *TX.lock() = Some(ring);
            *SETUP.lock() = Some(Setup {
                notify_base,
                mul,
                rx: rxq,
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
                arp_sent = send_arp(Mac(mac), Ipv4([10, 0, 2, 15]), GATEWAY, true);
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

    // The transmit ring: more frames than descriptors went through it, so completed
    // descriptors were reclaimed and reused; the announcement batch filled the ring and its
    // sender slept until a TX-completion interrupt woke it; and at the end every frame has
    // completed and the whole pool is free again.
    let (sent, reclaimed, free) = crate::cpu::without_interrupts(|| {
        TX.lock().as_ref().map_or((0, 0, 0), |ring| {
            (ring.sent, ring.reclaimed, ring.free.available())
        })
    });
    let tx_irqs = TX_IRQS.load(Ordering::Relaxed);
    let tx_wakes = TX_WAKES.load(Ordering::Relaxed);
    let tx_full = TX_FULL.load(Ordering::Relaxed);
    let tx_slept = TX_SLEPT.load(Ordering::Relaxed);
    let tx_ok = finished
        && tx_vector_ok
        && sent > N_TX as u64
        && reclaimed == sent
        && free == N_TX
        && tx_full > 0
        && tx_slept > 0
        && tx_wakes > 0
        && tx_irqs > 0;
    let _ = writeln!(
        com1,
        "origin-kernel: virtio-net TX: {sent} frames through {N_TX} descriptors (buffers allocated once), {reclaimed} completions reclaimed, {free} of {N_TX} free at the end; the ring was full {tx_full} times and senders slept {tx_slept} times, woken by the TX-completion interrupt {tx_wakes} (of {tx_irqs}); consistent {}{}",
        tx_ok,
        if tx_ok { "" } else { ": virtio-net failed" }
    );

    // Traffic on the lease: echo requests from the leased address to the gateway, unicast to
    // the MAC ARP resolved; every reply matched its request.
    let replies = PING_REPLIES.load(Ordering::Relaxed);
    let per_us = (tsc_hz / 1_000_000).max(1);
    let (min, avg, max) = match PING_TOTAL.load(Ordering::Relaxed).checked_div(replies) {
        Some(mean) => (
            PING_MIN.load(Ordering::Relaxed) / per_us,
            mean / per_us,
            PING_MAX.load(Ordering::Relaxed) / per_us,
        ),
        None => (0, 0, 0),
    };
    let ping_ok = finished && gateway != 0 && lease & (1 << 32) != 0 && replies == u64::from(PINGS);
    let _ = writeln!(
        com1,
        "origin-kernel: virtio-net ping: {}.{}.{}.{} -> 10.0.2.2 ({:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}): {replies} of {PINGS} echo replies (identifier, sequence and data matched); round trip min/avg/max {min}/{avg}/{max} us; consistent {}{}",
        ip[0],
        ip[1],
        ip[2],
        ip[3],
        g[2],
        g[3],
        g[4],
        g[5],
        g[6],
        g[7],
        ping_ok,
        if ping_ok { "" } else { ": virtio-net failed" }
    );

    // A file over the lease: the kernel's own initramfs image fetched from the gateway's
    // TFTP server must arrive whole and identical to the copy built into the kernel; and the
    // receive ring, which posts only N_RX buffers, must have recycled them to carry it.
    let bytes = TFTP_BYTES.load(Ordering::Relaxed);
    let blocks = TFTP_BLOCKS.load(Ordering::Relaxed);
    let complete = TFTP_COMPLETE.load(Ordering::Relaxed) != 0;
    let identical = TFTP_IDENTICAL.load(Ordering::Relaxed) != 0;
    let server_error = TFTP_ERROR.load(Ordering::Relaxed);
    let frames = RX_FRAMES.load(Ordering::Relaxed);
    let recycled = RX_RECYCLED.load(Ordering::Relaxed);
    let tftp_ok = finished
        && complete
        && identical
        && bytes == crate::INITRAMFS_IMAGE.len() as u64
        && frames > N_RX as u64
        && recycled > 0;
    let outcome = if identical {
        alloc::string::String::from("identical to the embedded image")
    } else if server_error != 0 {
        alloc::format!("server error {}", server_error - 1)
    } else if complete {
        alloc::string::String::from("differs from the embedded image")
    } else {
        alloc::string::String::from("incomplete")
    };
    let _ = writeln!(
        com1,
        "origin-kernel: virtio-net TFTP: {TFTP_FILE} from 10.0.2.2: {bytes} bytes in {blocks} blocks{}, {outcome}; the receive ring's {N_RX} buffers carried {frames} frames ({recycled} recycled); consistent {}{}",
        if complete && bytes.is_multiple_of(framekernel::tftp::BLOCK as u64) {
            " (the last empty)"
        } else {
            ""
        },
        tftp_ok,
        if tftp_ok { "" } else { ": virtio-net failed" }
    );
}
