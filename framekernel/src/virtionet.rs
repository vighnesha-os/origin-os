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
//! The ring memory is zeroed DMA pages (`dma::DeviceDma`). When the device offers
//! `VIRTIO_F_ACCESS_PLATFORM` its DMA goes through the IOMMU: the driver accepts the feature,
//! attaches the device to a domain of its own and gives it IOVAs (ADR-0007); otherwise the
//! device uses physical addresses (virtio 1.1 §6). x86 DMA is cache-coherent. The MSI-X
//! entries are programmed through `iommu::msi_message` (remappable handles when interrupts
//! are remapped). The `unsafe` here is volatile MMIO and volatile access to those DMA pages
//! (`DeviceMem`): the device can write them at any moment, so no reference ever covers them
//! and a received frame is copied out once, into kernel memory, before anything parses it.

use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering, fence};

use framekernel::net::{self, Arp, EthFrame, Ipv4, Mac, arp_op, ethertype};
use framekernel::pci::{self, Address};
use framekernel::virtio::{self, feature, status};
use framekernel::virtqueue::{self, Descriptor, F_WRITE, RingMemory};
use origin_sync::SpinLock;

use crate::console::Console;
use crate::dma::{DeviceDma, DmaPage};
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

/// The device, its MSI-X table (mapped) and the x2APIC ID its entries target, once set up.
static NET_MSIX: SpinLock<Option<(Address, u64, u32)>> = SpinLock::new(None);

/// The messages in the NIC's receive and transmit MSI-X entries, each with the vector and
/// the processor it must reach (for the interrupt-remapping proof); empty before bring-up.
pub(crate) fn msix_routes() -> alloc::vec::Vec<(Address, framekernel::vtd::MsiMessage, u8, u32)> {
    let Some((at, table, target)) = crate::cpu::without_interrupts(|| *NET_MSIX.lock()) else {
        return alloc::vec::Vec::new();
    };
    let word = |entry: u16, w: u64| {
        let p = ptr::with_exposed_provenance::<u32>(
            (table + u64::from(entry) * pci::MSIX_ENTRY + 4 * w) as usize,
        );
        // SAFETY: inside the MSI-X table (`enable_msix_table` checked it against its BAR and
        // mapped it uncached; both entries are below its entry count). Reading an entry has
        // no effect on the device.
        unsafe { p.read_volatile() }
    };
    [
        (RX_MSIX_ENTRY, crate::cpu::NET_VECTOR),
        (TX_MSIX_ENTRY, crate::cpu::NET_TX_VECTOR),
    ]
    .into_iter()
    .map(|(entry, vector)| {
        let message = framekernel::vtd::MsiMessage {
            address: u64::from(word(entry, 0)) | u64::from(word(entry, 1)) << 32,
            data: word(entry, 2),
        };
        (at, message, vector, target)
    })
    .collect()
}

/// Counts the virtio-net MSI-X interrupts the device has raised.
static NET_IRQS: AtomicU64 = AtomicU64::new(0);
/// The CPU index the last NIC interrupt arrived on, plus one (0: none yet).
static NET_IRQ_CPU: AtomicU64 = AtomicU64::new(0);
/// The receive thread sleeps here until a used-buffer interrupt wakes it.
static NET_WAIT: WaitQueue = WaitQueue::new();
/// NIC interrupts that found the receive thread asleep and woke it.
static NET_WAKES: AtomicU64 = AtomicU64::new(0);

/// Called from the NIC's interrupt vector (`cpu::NET_VECTOR`): records the used-buffer
/// interrupt and wakes the receive thread, which then reads the used ring itself.
pub(crate) fn on_interrupt() {
    NET_IRQS.fetch_add(1, Ordering::Relaxed);
    NET_IRQ_CPU.store(
        (crate::cpu::this_cpu().get() as u64).saturating_add(1),
        Ordering::Relaxed,
    );
    if NET_WAIT.wake_one() {
        NET_WAKES.fetch_add(1, Ordering::Relaxed);
    }
}

/// The bytes of the common configuration structure this driver uses: through the 64-bit
/// device-area address at 30h.
const COMMON_CONFIG_LEN: u32 = 56;
/// The bytes of the device configuration it reads: the MAC and the status word.
const DEVICE_CONFIG_LEN: u32 = 8;

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

/// The bytes of one DMA page.
const DMA_PAGE: usize = 4096;

/// One DMA page the device may write at any moment: a ring, or a frame buffer. The kernel
/// touches it only through the volatile accesses here, so no Rust reference ever covers
/// device-writable memory. A ring field is one aligned load or store of its own width, so it
/// cannot tear; frame bytes are copied once, into or out of kernel memory.
#[derive(Clone, Copy)]
struct DeviceMem {
    virt: u64,
}

impl DeviceMem {
    /// The page `DeviceDma::page` handed out (kept for ever, so the address stays valid).
    fn of(page: DmaPage) -> Self {
        Self { virt: page.virt }
    }

    /// Where a `T` at byte offset `at` lives, if it is inside the page and aligned.
    fn field<T>(self, at: usize) -> Option<*mut T> {
        let end = at.checked_add(size_of::<T>())?;
        (end <= DMA_PAGE && at.is_multiple_of(align_of::<T>()))
            .then(|| ptr::with_exposed_provenance_mut::<T>(self.virt as usize + at))
    }

    /// Reads a `T` at `at`; zero for one outside the page or misaligned. (`virtqueue` keeps
    /// every field inside a page and aligned.)
    fn load<T: Copy + Default>(self, at: usize) -> T {
        self.field::<T>(at).map_or_else(T::default, |p| {
            // SAFETY: `p` is aligned and inside this page (`field`), which stays allocated for
            // ever. A volatile load forms no reference; the device may write it concurrently.
            unsafe { p.read_volatile() }
        })
    }

    /// Writes a `T` at `at`, if that is inside the page and aligned.
    fn store<T: Copy>(self, at: usize, value: T) {
        if let Some(p) = self.field::<T>(at) {
            // SAFETY: as `load`; the kernel owns this field's value until it publishes it.
            unsafe { p.write_volatile(value) };
        }
    }

    /// Copies `out.len()` bytes from offset `at` into `out` with volatile reads; `false` if
    /// they are not all inside the page.
    fn copy_out(self, at: usize, out: &mut [u8]) -> bool {
        if at.checked_add(out.len()).is_none_or(|end| end > DMA_PAGE) {
            return false;
        }
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = self.load(at + i);
        }
        true
    }

    /// Copies `bytes` to offset `at` with volatile writes; `false` if they do not fit.
    fn copy_in(self, at: usize, bytes: &[u8]) -> bool {
        if at.checked_add(bytes.len()).is_none_or(|end| end > DMA_PAGE) {
            return false;
        }
        for (i, &byte) in bytes.iter().enumerate() {
            self.store(at + i, byte);
        }
        true
    }
}

impl RingMemory for DeviceMem {
    fn byte_len(&self) -> usize {
        DMA_PAGE
    }
    fn read_u16(&self, at: usize) -> u16 {
        u16::from_le(self.load(at))
    }
    fn read_u32(&self, at: usize) -> u32 {
        u32::from_le(self.load(at))
    }
    fn read_u64(&self, at: usize) -> u64 {
        u64::from_le(self.load(at))
    }
    fn write_u16(&mut self, at: usize, value: u16) {
        self.store(at, value.to_le());
    }
    fn write_u32(&mut self, at: usize, value: u32) {
        self.store(at, value.to_le());
    }
    fn write_u64(&mut self, at: usize, value: u64) {
        self.store(at, value.to_le());
    }
}

/// One set-up virtqueue.
#[derive(Clone, Copy)]
struct Queue {
    size: usize,
    desc: DeviceMem,
    avail: DeviceMem,
    used: DeviceMem,
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
    /// How the device reaches memory: the receive buffers are mapped through it.
    dma: DeviceDma,
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
    /// Each descriptor's buffer: (where the kernel reaches it, bus address).
    bufs: [(DeviceMem, u64); N_TX],
    free: virtqueue::FreeList,
    in_flight: [bool; N_TX],
    /// The used ring is consumed up to here.
    last_used: u16,
    /// The device broke the ring's rules: nothing more is sent or reclaimed.
    fault: bool,
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
/// How often the device claimed more completions than a ring holds.
static DEVICE_FAULTS: AtomicU64 = AtomicU64::new(0);

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

/// Maps the BAR holding capability `loc` and returns the virtual address of its structure,
/// which must lie inside that memory BAR and be at least `min` bytes long: the offset and
/// length are the device's, and the driver reads and writes through this address.
fn map_structure(dev: &crate::pcie::Device, loc: virtio::CfgLocation, min: u32) -> Option<u64> {
    let bar = dev.bars.get(usize::from(loc.bar))?.as_ref()?;
    let address = loc.address_in(bar, min)?;
    let base = crate::vm::map_mmio(bar.address, bar.size).ok()?;
    Some(base + (address - bar.address))
}

/// Sets up queue `q`: a smaller-of device size and 256 ring in three DMA pages (from `dma`),
/// with their bus addresses programmed, its MSI-X `vector` assigned (`NO_VECTOR` for none), and
/// the queue enabled. MSI-X must already be enabled on the function for a real vector to
/// take. `None` if the device offers no queue, one whose size is not a power of two, or a
/// notification register outside its notification structure `notify`.
fn setup_queue(
    common: u64,
    q: u16,
    vector: u16,
    dma: DeviceDma,
    notify: virtio::CfgLocation,
    mul: u32,
) -> Option<Queue> {
    w16(common + reg::QUEUE_SELECT, q);
    let size = virtqueue::queue_size(r16(common + reg::QUEUE_SIZE), 256)?;
    w16(common + reg::QUEUE_SIZE, size as u16);
    let notify_off = r16(common + reg::QUEUE_NOTIFY_OFF);
    notify.notify_offset(notify_off, mul)?;
    let (d, a, u) = (dma.page()?, dma.page()?, dma.page()?);
    w64(common + reg::QUEUE_DESC, d.bus);
    w64(common + reg::QUEUE_DRIVER, a.bus);
    w64(common + reg::QUEUE_DEVICE, u.bus);
    w16(common + reg::QUEUE_MSIX_VECTOR, vector);
    w16(common + reg::QUEUE_ENABLE, 1);
    Some(Queue {
        size,
        desc: DeviceMem::of(d),
        avail: DeviceMem::of(a),
        used: DeviceMem::of(u),
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
    let Some(table_address) = msix.table_address(&bar) else {
        return false;
    };
    let Ok(table) = crate::vm::map_mmio(table_address, table_len) else {
        return false;
    };
    let at = dev.function.at;
    let (Ok(rx_message), Ok(tx_message)) = (
        crate::iommu::msi_message(at, target, crate::cpu::NET_VECTOR),
        crate::iommu::msi_message(at, target, crate::cpu::NET_TX_VECTOR),
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
    for (index, message) in [(RX_MSIX_ENTRY, rx_message), (TX_MSIX_ENTRY, tx_message)] {
        let i = u64::from(index);
        entry(i, 0, message.address as u32);
        entry(i, 1, (message.address >> 32) as u32);
        entry(i, 2, message.data);
        entry(i, 3, 0); // unmask
    }
    crate::cpu::without_interrupts(|| {
        *NET_MSIX.lock() = Some((at, table, target));
    });
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
/// `i`), returning those buffers, then notifies the device.
fn post_rx(rx: &Queue, notify_base: u64, mul: u32, dma: DeviceDma) -> Option<[DeviceMem; N_RX]> {
    // The queue's size is the device's: a shorter ring has no descriptor `i` for every buffer.
    if rx.size < N_RX {
        return None;
    }
    let mut pages = [DeviceMem { virt: 0 }; N_RX];
    let (mut desc, mut avail) = (rx.desc, rx.avail);
    for (i, slot) in pages.iter_mut().enumerate() {
        let page = dma.page()?;
        *slot = DeviceMem::of(page);
        Descriptor {
            addr: page.bus,
            len: RX_BUF as u32,
            flags: F_WRITE,
            next: 0,
        }
        .write(&mut desc, i);
        virtqueue::avail_push(&mut avail, rx.size, i as u16);
    }
    notify(notify_base, rx.notify_off, mul, Q_RX);
    Some(pages)
}

impl TxRing {
    /// A ring over `q` with one buffer page, allocated now and kept, for each of its first
    /// [`N_TX`] descriptors.
    fn new(q: Queue, notify_base: u64, mul: u32, dma: DeviceDma) -> Option<Self> {
        if q.size < N_TX {
            return None;
        }
        let mut bufs = [(DeviceMem { virt: 0 }, 0); N_TX];
        for buf in &mut bufs {
            let page = dma.page()?;
            *buf = (DeviceMem::of(page), page.bus);
        }
        Some(Self {
            q,
            notify_base,
            mul,
            bufs,
            free: virtqueue::FreeList::new(N_TX),
            in_flight: [false; N_TX],
            last_used: 0,
            fault: false,
            sent: 0,
            reclaimed: 0,
        })
    }

    /// Whether the device has completed frames that are not reclaimed yet.
    fn completion_pending(&self) -> bool {
        virtqueue::used_idx(&self.q.used) != self.last_used
    }

    /// Returns completed frames' descriptors to the free list: at most a ring's worth per
    /// call, however the device moves its index, since this runs with interrupts off. The
    /// device reports the ids: one out of range or not in flight is consumed but never freed
    /// (so never twice). An index that claims more than the ring holds is a device fault, and
    /// the ring is finished.
    fn reclaim(&mut self) {
        if self.fault {
            return;
        }
        let reaped = virtqueue::reap(
            &self.q.used,
            self.q.size,
            &mut self.last_used,
            |id, _len| {
                if let Some(busy) = self.in_flight.get_mut(id as usize)
                    && *busy
                {
                    *busy = false;
                    self.free.free(id as u16);
                    self.reclaimed += 1;
                }
            },
        );
        if reaped.is_err() {
            self.fault = true;
            DEVICE_FAULTS.fetch_add(1, Ordering::Relaxed);
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
        let Some(&(buffer, bus)) = self.bufs.get(i) else {
            self.free.free(id); // cannot happen: the free list holds only 0..N_TX
            return false;
        };
        // The descriptor is free, so the device is done with this buffer.
        let hdr = virtio::NetHeader::plain().encode();
        let total = hdr.len() + frame.len();
        if !(buffer.copy_in(0, &hdr) && buffer.copy_in(hdr.len(), frame)) {
            self.free.free(id); // cannot happen: `transmit_with` checked the frame fits
            return false;
        }
        Descriptor {
            addr: bus,
            len: total as u32,
            flags: 0,
            next: 0,
        }
        .write(&mut self.q.desc, i);
        if let Some(busy) = self.in_flight.get_mut(i) {
            *busy = true;
        }
        virtqueue::avail_push(&mut self.q.avail, self.q.size, id);
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

/// Sleeps on [`TX_WAIT`] until a TX completion is pending (see [`tx_kick_then_pending`]);
/// `false` once `deadline` (monotonic nanoseconds) has passed. Checked first as well as
/// inside the wait, so a device that always reports another completion cannot hold a
/// transmitter past it.
fn tx_sleep_for_completion(deadline: u64) -> bool {
    if crate::time::monotonic_nanos() >= deadline {
        return false;
    }
    match TX_WAIT.wait_until_deadline(tx_kick_then_pending, deadline) {
        Ok(blocked) => {
            TX_SLEPT.fetch_add(blocked, Ordering::Relaxed);
            true
        }
        Err(_) => false,
    }
}

/// Transmits `frame` (a complete Ethernet frame) through the TX ring, kicking the device now
/// when `kick`, or leaving it queued for a later kick (a batch). It reclaims completed
/// descriptors first, and while every descriptor is in flight sleeps on [`TX_WAIT`] until
/// the NIC's TX-completion interrupt reports one done (a kernel thread sleeps; the boot flow
/// yields), for at most [`REPLY_TIMEOUT_NS`]. Returns whether the frame was queued.
fn transmit_with(frame: &[u8], kick: bool) -> bool {
    if virtio::NET_HDR_LEN + frame.len() > DMA_PAGE {
        return false;
    }
    let deadline = reply_deadline();
    loop {
        let queued = crate::cpu::without_interrupts(|| {
            let mut ring = TX.lock();
            let ring = ring.as_mut()?;
            ring.reclaim();
            (!ring.fault).then(|| ring.enqueue(frame, kick))
        });
        match queued {
            None => return false,
            Some(true) => return true,
            Some(false) => {
                TX_FULL.fetch_add(1, Ordering::Relaxed);
                if !tx_sleep_for_completion(deadline) {
                    return false;
                }
            }
        }
    }
}

/// Transmits `frame` and kicks the device at once.
fn transmit(frame: &[u8]) -> bool {
    transmit_with(frame, true)
}

/// Kicks the device and sleeps on [`TX_WAIT`] until every queued frame has completed and
/// its descriptor is back in the pool, or [`REPLY_TIMEOUT_NS`] has passed (or the device has
/// faulted): what is still in flight then shows in the TX report.
fn tx_flush() {
    let deadline = reply_deadline();
    loop {
        let idle = crate::cpu::without_interrupts(|| {
            TX.lock().as_mut().is_none_or(|ring| {
                ring.reclaim();
                ring.fault || ring.free.available() == N_TX
            })
        });
        if idle || !tx_sleep_for_completion(deadline) {
            return;
        }
    }
}

/// The receive ring: buffers `0..N_RX` (kept DMA pages, posted once by [`post_rx`]) that
/// the device fills, recycled one at a time — the buffer holding the frame [`RxRing::recv`]
/// returned goes back to the device at the next `recv`, so the ring never runs dry.
struct RxRing {
    q: Queue,
    pages: [DeviceMem; N_RX],
    notify_base: u64,
    mul: u32,
    /// The used ring is consumed up to here.
    last_used: u16,
    /// Each buffer is the device's to fill (posted) or the driver's (returned, not yet
    /// re-posted): a device-reported id that is not posted is refused, never re-posted twice.
    posted: [bool; N_RX],
    /// The buffer holding the frame the last `recv` returned.
    held: Option<u16>,
    /// Where each received frame is copied, once, for everything after `recv` to parse: the
    /// device cannot write here.
    frame: [u8; RX_BUF],
    /// The device broke the ring's rules: nothing more is read from it.
    fault: bool,
    received: u64,
    recycled: u64,
}

impl RxRing {
    /// The ring over `q` whose buffers `post_rx` posted at `pages`.
    fn new(q: Queue, pages: [DeviceMem; N_RX], notify_base: u64, mul: u32) -> Self {
        Self {
            q,
            pages,
            notify_base,
            mul,
            last_used: 0,
            posted: [true; N_RX],
            held: None,
            frame: [0; RX_BUF],
            fault: false,
            received: 0,
            recycled: 0,
        }
    }

    /// Hands the previous frame's buffer back to the device, then sleeps until the next
    /// received frame, copies it once into kernel memory and returns its Ethernet bytes (the
    /// virtio-net header stripped), or `None` by `deadline` (monotonic nanoseconds), whether
    /// nothing came or the device keeps reporting frames, and from then on if the device
    /// breaks the ring's rules. The slice is valid until the next `recv` (the borrow of
    /// `self` enforces it). An empty slice means a malformed completion the caller should
    /// skip.
    ///
    /// The running kernel thread blocks on [`NET_WAIT`]; the NIC's used-buffer interrupt
    /// (`on_interrupt`) wakes it as a frame arrives, and the deadline is only a backstop for
    /// a frame that never comes (a timer wake-up, within one 10 ms tick of it). The condition
    /// reads the used ring itself, so a frame that arrived before the thread registered is
    /// seen at once and no wake-up is lost.
    fn recv(&mut self, deadline: u64) -> Option<&[u8]> {
        if self.fault {
            return None;
        }
        if let Some(id) = self.held.take() {
            // Its descriptor still describes the same page, so publishing the id is enough.
            if let Some(posted) = self.posted.get_mut(usize::from(id)) {
                *posted = true;
            }
            virtqueue::avail_push(&mut self.q.avail, self.q.size, id);
            notify(self.notify_base, self.q.notify_off, self.mul, Q_RX);
            self.recycled += 1;
        }
        // Checked first as well as inside the wait: otherwise a device whose index always
        // shows another frame would keep a caller looping past its deadline.
        if crate::time::monotonic_nanos() >= deadline {
            return None;
        }
        let (used, last) = (self.q.used, self.last_used);
        let blocked = NET_WAIT
            .wait_until_deadline(|| virtqueue::used_idx(&used) != last, deadline)
            .ok()?;
        RX_BLOCKED.fetch_add(blocked, Ordering::Relaxed);
        // The index moved, so a conforming device is now between 1 and a ring's worth ahead.
        if !matches!(
            virtqueue::used_advance(virtqueue::used_idx(&used), last, self.q.size),
            Ok(1..)
        ) {
            self.fault = true;
            DEVICE_FAULTS.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        fence(Ordering::SeqCst);
        let slot = usize::from(self.last_used) % self.q.size;
        let (id, len) = virtqueue::used_elem(&used, slot);
        self.last_used = self.last_used.wrapping_add(1);
        self.received += 1;
        let (i, len) = (id as usize, len as usize);
        let (Some(posted), Some(&buffer)) = (self.posted.get_mut(i), self.pages.get(i)) else {
            return Some(&[]); // an id we never posted: nothing to read or recycle
        };
        if !*posted {
            return Some(&[]); // a buffer the device already returned: refuse it
        }
        *posted = false;
        self.held = Some(id as u16);
        if !(virtio::NET_HDR_LEN..=RX_BUF).contains(&len) {
            return Some(&[]);
        }
        let frame = self.frame.get_mut(..len - virtio::NET_HDR_LEN)?;
        buffer
            .copy_out(virtio::NET_HDR_LEN, frame)
            .then_some(&*frame)
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
fn transmit_dhcp(our_mac: Mac, dhcp: &[u8], damage: bool) -> bool {
    let any = Ipv4([0, 0, 0, 0]);
    let bcast = Ipv4([255, 255, 255, 255]);
    let mut udp = [0u8; 640];
    let Some(un) = net::Udp::write(
        any,
        bcast,
        framekernel::dhcp::CLIENT_PORT,
        framekernel::dhcp::SERVER_PORT,
        dhcp,
        &mut udp,
    ) else {
        return false;
    };
    if damage {
        // A deliberately corrupted UDP checksum: the server must discard the datagram.
        udp[7] ^= 0xFF;
    }
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

/// QEMU's user-networking gateway, the address the datapath resolves by ARP.
const GATEWAY: Ipv4 = Ipv4([10, 0, 2, 2]);
/// The address the boot's ARP request for [`GATEWAY`] names as its sender: QEMU's user
/// networking gives its first guest this address, and the lease comes only later.
const ARP_REQUEST_SENDER: Ipv4 = Ipv4([10, 0, 2, 15]);

/// The ARP request (operation 1) from `sender_ip` for `target_ip`.
fn arp_request(our_mac: Mac, sender_ip: Ipv4, target_ip: Ipv4) -> Arp {
    Arp {
        operation: arp_op::REQUEST,
        sender_mac: our_mac,
        sender_ip,
        target_mac: Mac([0; 6]),
        target_ip,
    }
}

/// Transmits an ARP request (a broadcast Ethernet frame) asking for `target_ip`, from
/// `sender_ip`, kicking the device at once when `kick` (else it joins a batch). With both
/// addresses equal it is a gratuitous announcement (RFC 5227 §2.3).
fn send_arp(our_mac: Mac, sender_ip: Ipv4, target_ip: Ipv4, kick: bool) -> bool {
    let mut req = [0u8; net::ARP_LEN];
    if arp_request(our_mac, sender_ip, target_ip)
        .write(&mut req)
        .is_none()
    {
        return false;
    }
    let mut frame = [0u8; net::ETH_HEADER + net::ARP_LEN];
    let Some(fl) = build_frame(&mut frame, Mac::BROADCAST, our_mac, ethertype::ARP, &req) else {
        return false;
    };
    transmit_with(&frame[..fl], kick)
}

/// Sleeps for received frames until the reply to `request` arrives, however many others
/// come first, or its deadline passes; returns the MAC it gives. `Arp::answer_in` checks the
/// reply against the request.
fn await_arp_reply(rx: &mut RxRing, request: &Arp) -> Option<Mac> {
    let deadline = reply_deadline();
    loop {
        let Some(eth) = rx.recv(deadline) else {
            timed_out(waiting::ARP, 0);
            return None;
        };
        if let Some(mac) = request.answer_in(eth) {
            return Some(mac);
        }
    }
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
/// `from` to `to`, however many others come first, or its deadline passes. The reply must
/// carry our identifier, the sequence and the request's data, with valid IPv4 and ICMP
/// checksums.
fn await_echo_reply(rx: &mut RxRing, from: Ipv4, to: Ipv4, sequence: u16) -> bool {
    let data = ping_data(sequence);
    let deadline = reply_deadline();
    loop {
        let Some(eth) = rx.recv(deadline) else {
            timed_out(waiting::ECHO, u64::from(sequence));
            return false;
        };
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
}

/// Pings the gateway [`PINGS`] times from the leased address `ours`, unicast to the MAC the
/// ARP exchange resolved, sleeping on the receive interrupt for each reply, and records the
/// replies that matched and their round-trip times.
fn ping_gateway(rx: &mut RxRing, our_mac: Mac, gateway_mac: Mac, ours: Ipv4) {
    for sequence in 1..=PINGS {
        let start = crate::cpu::rdtsc();
        if !(send_ping(our_mac, gateway_mac, ours, GATEWAY, sequence)
            && await_echo_reply(rx, GATEWAY, ours, sequence))
        {
            // No reply (the wait timed out, or nothing matched): stop pinging a gateway that
            // is not answering, and let the datapath go on.
            break;
        }
        let rtt = crate::cpu::rdtsc().wrapping_sub(start);
        PING_REPLIES.fetch_add(1, Ordering::Relaxed);
        PING_MIN.fetch_min(rtt, Ordering::Relaxed);
        PING_MAX.fetch_max(rtt, Ordering::Relaxed);
        PING_TOTAL.fetch_add(rtt, Ordering::Relaxed);
    }
}

/// The file fetched over TFTP: `xtask` points slirp's built-in TFTP server (`tftp=`) at
/// `framekernel/assets`, the directory the kernel's own initramfs image comes from.
const TFTP_FILE: &str = "initramfs.tar";
/// The largest file the fetch accepts.
const TFTP_LIMIT: usize = 64 * 1024;
/// How long the reader waits for each packet before retransmitting what it sent last, and
/// how many attempts it makes before giving up: together the 2 s budget of any other reply.
const TFTP_ATTEMPT_NS: u64 = 500_000_000;
const TFTP_ATTEMPTS: u32 = 4;
/// The whole transfer's budget: far more than the largest file needs, and short of the boot
/// flow's 10 s wait, so a server that keeps a transfer alive with repeats still ends it.
const TFTP_TRANSFER_NS: u64 = 8_000_000_000;
/// The boot proof drops the first read request on purpose — it goes out with a corrupted UDP
/// checksum, which the server discards — so the reader's timeout-and-retransmit path runs on
/// every boot, and the file must still arrive whole.
const TFTP_INJECT_LOST_REQUEST: bool = true;
/// Packets the reader retransmitted after a timeout.
static TFTP_RETRANSMITS: AtomicU64 = AtomicU64::new(0);

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
/// The file the TFTP fetch completed, handed to the boot flow, which unpacks it.
static FETCHED: SpinLock<Option<alloc::vec::Vec<u8>>> = SpinLock::new(None);

/// How long the receive thread waits for each expected reply before giving up on it. A
/// reply normally takes well under a millisecond (QEMU TCG), so this only ends a wait for
/// a reply that is not coming — quickly, and saying which.
const REPLY_TIMEOUT_NS: u64 = 2_000_000_000;

/// What the receive thread was waiting for when a wait timed out, or why it stopped short of
/// a wait: the step in the high half, a detail (an echo sequence, a TFTP block) in the low
/// half; 0 when neither happened. The first one wins.
static TIMED_OUT: AtomicU64 = AtomicU64::new(0);

/// The steps whose replies the receive thread waits for.
mod waiting {
    pub(super) const ARP: u64 = 1;
    pub(super) const DHCP: u64 = 2;
    pub(super) const ECHO: u64 = 3;
    pub(super) const TFTP: u64 = 4;
    /// Not a wait: no hardware entropy for a transaction ID or port.
    pub(super) const NO_ENTROPY: u64 = 5;
    /// Not a wait: the DHCP server refused every restart.
    pub(super) const DHCP_REFUSED: u64 = 6;
}

/// How long the DHCP exchange waits for each reply before retransmitting its last message,
/// and how many attempts it makes before giving up: together the 2 s budget of any other
/// reply. (RFC 2131 §4.1 asks for randomized exponential backoff from 4 s; a fixed short
/// attempt suits a boot-time proof on an emulated link.)
const DHCP_ATTEMPT_NS: u64 = 500_000_000;
const DHCP_ATTEMPTS: u32 = 4;
/// The boot proof drops the first DISCOVER on purpose — it goes out with a corrupted UDP
/// checksum, which the server discards — so the exchange's retransmit path runs on every
/// boot, and the lease must still be bound.
const DHCP_INJECT_LOST_DISCOVER: bool = true;
/// DHCP messages retransmitted after a timeout.
static DHCP_RETRANSMITS: AtomicU64 = AtomicU64::new(0);

/// The deadline for a reply awaited from now.
fn reply_deadline() -> u64 {
    crate::time::monotonic_nanos().saturating_add(REPLY_TIMEOUT_NS)
}

/// 64 bits of hardware entropy (RDSEED, else RDRAND, with `entropy::draw`'s bounded retries)
/// for an identifier an off-path sender must not be able to guess: a DHCP transaction ID, a
/// TFTP port. `None` when the processor gives none; the caller stops rather than use a
/// predictable value.
fn random_u64() -> Option<u64> {
    use framekernel::entropy::{self, Source};
    use framekernel::features::Features;

    struct Hardware(Features);
    impl Source for Hardware {
        fn try_next(&mut self) -> Option<u64> {
            if self.0.rdseed
                && let Some(v) = crate::cpu::rdseed()
            {
                return Some(v);
            }
            if self.0.rdrand {
                crate::cpu::rdrand()
            } else {
                None
            }
        }
    }

    let features = Features::decode(&crate::cpu::gather_cpuid());
    entropy::draw(&mut Hardware(features), entropy::RDRAND_RETRIES)
}

/// Records why the receive thread stopped waiting for `step` (with `detail`), unless an
/// earlier one did.
fn timed_out(step: u64, detail: u64) {
    let _ = TIMED_OUT.compare_exchange(
        0,
        (step << 32) | (detail & 0xFFFF_FFFF),
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
}

/// The note a report line carries when the wait for `step` timed out (else empty).
fn timeout_note(step: u64) -> alloc::string::String {
    let t = TIMED_OUT.load(Ordering::Relaxed);
    if t >> 32 != step {
        return alloc::string::String::new();
    }
    let detail = t & 0xFFFF_FFFF;
    let what = match step {
        waiting::ARP => alloc::string::String::from("the gateway's ARP reply"),
        waiting::DHCP => {
            return alloc::format!(
                "; timed out after {DHCP_ATTEMPTS} attempts of {} ms waiting for a DHCP reply",
                DHCP_ATTEMPT_NS / 1_000_000
            );
        }
        waiting::ECHO => alloc::format!("the reply to echo request {detail}"),
        waiting::NO_ENTROPY => {
            return alloc::string::String::from(
                "; no hardware entropy for a transaction ID or port",
            );
        }
        waiting::DHCP_REFUSED => {
            return alloc::format!(
                "; the DHCP server refused the lease through {} restarts",
                framekernel::dhcp::MAX_RESTARTS
            );
        }
        _ => {
            return alloc::format!(
                "; timed out after {TFTP_ATTEMPTS} attempts of {} ms waiting for TFTP block {detail}",
                TFTP_ATTEMPT_NS / 1_000_000
            );
        }
    };
    alloc::format!(
        "; timed out after {} ms waiting for {what}",
        REPLY_TIMEOUT_NS / 1_000_000
    )
}

/// Transmits a UDP datagram from `src`:`sport` to `dst`:`dport` (with its checksum), unicast
/// to `dst_mac`; with `damage`, its checksum is corrupted on purpose (a loss injected for a
/// proof).
fn send_udp(
    our_mac: Mac,
    dst_mac: Mac,
    (src, sport): (Ipv4, u16),
    (dst, dport): (Ipv4, u16),
    payload: &[u8],
    damage: bool,
) -> bool {
    let mut udp = [0u8; net::UDP_HEADER + 600];
    let Some(un) = net::Udp::write(src, dst, sport, dport, payload, &mut udp) else {
        return false;
    };
    if damage {
        // A deliberately corrupted UDP checksum: the receiver must discard the datagram.
        udp[7] ^= 0xFF;
    }
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

/// Sleeps for received frames until a UDP datagram from `from` to `to`:`port` arrives,
/// however many others come first, copies its payload into `out` and returns its source port
/// and length; `None` once `deadline` (monotonic nanoseconds) passes. The IPv4 header
/// checksum and the UDP checksum are verified; a datagram whose payload does not fit `out`
/// is skipped.
fn await_udp(
    rx: &mut RxRing,
    (from, to, port): (Ipv4, Ipv4, u16),
    out: &mut [u8],
    deadline: u64,
) -> Option<(u16, usize)> {
    loop {
        let eth = rx.recv(deadline)?;
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
}

/// Fetches [`TFTP_FILE`] from the TFTP server on the gateway (`tftp::Download` drives the
/// protocol): a read request to port 69 from the leased address and a random client port,
/// then each DATA block acknowledged to the port the server answers from, until the short
/// final block — the thread sleeping on the receive interrupt for every block. Records the
/// outcome and whether the file matches the image built into the kernel; only then is it
/// handed on to be unpacked.
fn fetch_tftp(rx: &mut RxRing, our_mac: Mac, server_mac: Mac, ours: Ipv4) {
    use framekernel::tftp;

    let mut request = [0u8; 64];
    let Some(n) = tftp::read_request(TFTP_FILE, &mut request) else {
        return;
    };
    let Some(random) = random_u64() else {
        timed_out(waiting::NO_ENTROPY, 0);
        return;
    };
    let client_port = tftp::client_port(random);
    let ours_port = (ours, client_port);
    // What was sent last and to which port: retransmitted when nothing comes back in time
    // (RFC 1350: the side waiting for a packet times out and retransmits its last one).
    let (mut last, mut last_len, mut last_port) = (request, n, tftp::PORT);
    if !send_udp(
        our_mac,
        server_mac,
        ours_port,
        (GATEWAY, last_port),
        &last[..last_len],
        TFTP_INJECT_LOST_REQUEST,
    ) {
        return;
    }
    let mut download = tftp::Download::new(TFTP_LIMIT);
    let mut server_port = None;
    let mut packet = [0u8; 4 + tftp::BLOCK];
    let attempt = || crate::time::monotonic_nanos().saturating_add(TFTP_ATTEMPT_NS);
    let transfer = crate::time::monotonic_nanos().saturating_add(TFTP_TRANSFER_NS);
    // A packet that gets an answer (new data, or a repeat of a block, which must be acknowledged
    // again) buys a new attempt; anything else (a stray, a packet from the wrong port) only uses
    // up the current one. The whole transfer is bounded by `TFTP_TRANSFER_NS` either way.
    let (mut deadline, mut misses) = (attempt(), 0);
    while crate::time::monotonic_nanos() < transfer {
        let Some((port, len)) = await_udp(rx, (GATEWAY, ours, client_port), &mut packet, deadline)
        else {
            misses += 1;
            if misses >= TFTP_ATTEMPTS {
                timed_out(waiting::TFTP, u64::from(download.blocks()) + 1);
                break;
            }
            TFTP_RETRANSMITS.fetch_add(1, Ordering::Relaxed);
            send_udp(
                our_mac,
                server_mac,
                ours_port,
                (GATEWAY, last_port),
                &last[..last_len],
                false,
            );
            deadline = attempt();
            continue;
        };
        // The transfer continues with the port the server first answered from (its TID).
        if *server_port.get_or_insert(port) != port {
            continue;
        }
        match download.on_packet(&packet[..len]) {
            tftp::Step::Ack {
                block,
                last: final_block,
            } => {
                (deadline, misses) = (attempt(), 0);
                if let Some(an) = tftp::ack(block, &mut last) {
                    (last_len, last_port) = (an, port);
                    send_udp(
                        our_mac,
                        server_mac,
                        ours_port,
                        (GATEWAY, port),
                        &last[..an],
                        false,
                    );
                }
                if final_block {
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
    // Only bytes identical to the expected image go on to be parsed as an initramfs.
    if identical {
        let file = download.into_data();
        crate::cpu::without_interrupts(|| *FETCHED.lock() = Some(file));
    }
}

/// Obtains a DHCP lease over the NIC: DISCOVER → OFFER → REQUEST → ACK with QEMU's built-in
/// server, returning the leased address. The transaction ID is random. A NAK starts over as a
/// new transaction, a bounded number of times; every wait is bounded by its attempt deadline,
/// and an attempt is renewed only by a retransmission or a step forward, whatever else
/// arrives.
fn dhcp_lease(rx: &mut RxRing, our_mac: Mac) -> Option<Ipv4> {
    use framekernel::dhcp::{self, Action};

    /// A new transaction ID, or `None` (recorded) with no entropy to draw one from.
    fn new_xid() -> Option<u32> {
        let random = random_u64();
        if random.is_none() {
            timed_out(waiting::NO_ENTROPY, 0);
        }
        random.map(|r| r as u32)
    }

    let mut client = dhcp::Client::new(our_mac.0);
    // The message sent last (the DISCOVER, then the REQUEST): retransmitted when no reply
    // comes within an attempt.
    let mut last = client.start(new_xid()?);
    transmit_dhcp(our_mac, &last, DHCP_INJECT_LOST_DISCOVER);
    let attempt = || crate::time::monotonic_nanos().saturating_add(DHCP_ATTEMPT_NS);
    let (mut deadline, mut misses) = (attempt(), 0);
    loop {
        let Some(eth) = rx.recv(deadline) else {
            misses += 1;
            if misses >= DHCP_ATTEMPTS {
                timed_out(waiting::DHCP, 0);
                return None;
            }
            DHCP_RETRANSMITS.fetch_add(1, Ordering::Relaxed);
            transmit_dhcp(our_mac, &last, false);
            deadline = attempt();
            continue;
        };
        let Some(message) = dhcp::message_in_frame(eth, our_mac.0) else {
            continue;
        };
        match client.on_receive(message) {
            Action::Send(request) => {
                transmit_dhcp(our_mac, &request, false);
                last = request;
                (deadline, misses) = (attempt(), 0);
            }
            Action::Bound(lease) => return Some(lease.address),
            Action::Restart => {
                let Some(discover) = client.restart(new_xid()?) else {
                    timed_out(waiting::DHCP_REFUSED, 0);
                    return None;
                };
                transmit_dhcp(our_mac, &discover, false);
                last = discover;
                (deadline, misses) = (attempt(), 0);
            }
            Action::Ignore => {}
        }
    }
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
        && let Some(rx_pages) = post_rx(&s.rx, s.notify_base, s.mul, s.dma)
    {
        let mut rx = RxRing::new(s.rx, rx_pages, s.notify_base, s.mul);
        RX_ARMED.store(1, Ordering::Release);
        let gateway = await_arp_reply(
            &mut rx,
            &arp_request(Mac(s.mac), ARP_REQUEST_SENDER, GATEWAY),
        );
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

/// Polls the status register for 0 after a reset, with a bound: a device may take a moment, and
/// one that never finishes must not hang the boot.
fn reset_completed(common: u64) -> bool {
    const POLLS: u32 = 1 << 20;
    (0..POLLS).any(|_| {
        core::hint::spin_loop();
        r8(common + reg::DEVICE_STATUS) == 0
    })
}

/// Brings up the virtio-net device, hands its queues to a receive thread on another
/// processor (the boot one when there is no other), and reports the result (CLAUDE.md §54).
pub(crate) fn prove(com1: &mut Console, cpus: &[crate::cpu::CpuIndex], tsc_hz: u64) {
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
    // Memory-space decoding for the registers. Bus mastering waits until the device has a
    // DMA path (ADR-0007 I12).
    if crate::pcie::enable_memory(at).is_none() {
        let _ = writeln!(
            com1,
            "origin-kernel: virtio-net: memory decoding did not turn on: virtio-net failed"
        );
        return;
    }

    // Only structures in a memory BAR and inside it: this driver has no port I/O paths, and
    // a device may list an I/O-BAR instance first (virtio 1.1 §4.1.4.1).
    let bars = dev.bars;
    let caps = virtio::find_caps(
        |off| crate::pcie::read_config32(at, off).unwrap_or(0),
        |loc| {
            bars.get(usize::from(loc.bar))
                .copied()
                .flatten()
                .is_some_and(|bar| loc.address_in(&bar, 1).is_some())
        },
    );
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
        map_structure(&dev, common_loc, COMMON_CONFIG_LEN),
        map_structure(&dev, notify_loc, 2),
        map_structure(&dev, dev_loc, DEVICE_CONFIG_LEN),
    ) else {
        let _ = writeln!(
            com1,
            "origin-kernel: virtio-net: cannot map BARs: virtio-net failed"
        );
        return;
    };

    // Status handshake: reset, acknowledge, driver. A device that does not read back 0 has not
    // finished resetting (virtio 1.1 §2.1.2) and cannot be called idle below.
    w8(common + reg::DEVICE_STATUS, 0);
    if !reset_completed(common) {
        let _ = writeln!(
            com1,
            "origin-kernel: virtio-net: the device did not finish its reset: virtio-net failed"
        );
        return;
    }
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
    let wanted =
        feature::VERSION_1 | feature::NET_MAC | feature::NET_STATUS | feature::ACCESS_PLATFORM;
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
    if !features_ok {
        // A device that refuses the features is unusable (virtio 1.1 §3.1.1): no bus mastering.
        let _ = writeln!(
            com1,
            "origin-kernel: virtio-net: the device refused the negotiated features: virtio-net failed"
        );
        return;
    }

    // Behind the IOMMU (ACCESS_PLATFORM accepted), the device gets a domain of its own
    // before its first DMA page; otherwise it uses physical addresses (virtio 1.1 §6).
    let dma = if accepted & feature::ACCESS_PLATFORM != 0 {
        match DeviceDma::attach(at) {
            Ok(d) => d,
            Err(why) => {
                let _ = writeln!(com1, "origin-kernel: virtio-net: {why}: virtio-net failed");
                return;
            }
        }
    } else {
        DeviceDma::Direct
    };
    // Bus mastering only now (ADR-0007 I12): the driver reset the device above, so it is idle
    // and owns no queue yet. A device without VIRTIO_F_ACCESS_PLATFORM behind a translating
    // IOMMU is refused here: it could not be confined.
    match dma.enable_bus_master(at) {
        Ok(command) => {
            let _ = writeln!(
                com1,
                "origin-kernel: virtio-net: bus mastering on after the device got its DMA path: command register {command:#06x}"
            );
        }
        Err(why) => {
            let _ = writeln!(com1, "origin-kernel: virtio-net: {why}: virtio-net failed");
            return;
        }
    }

    // The receive thread runs on another processor when there is one (as the NVMe interrupt
    // proof does), and the NIC's MSI-X message is routed to that processor, so it idles until
    // the interrupt wakes it. Turn MSI-X on (so the queues can take vectors), with no
    // config-change interrupt, then set up the receive queue (interrupt on entry
    // RX_MSIX_ENTRY) and the transmit queue (no interrupt; its completion is polled).
    let bsp = crate::cpu::this_cpu();
    let target = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let msix_enabled = target
        .apic_id()
        .is_some_and(|apic_id| enable_msix_table(&dev, apic_id));
    w16(common + reg::CONFIG_MSIX_VECTOR, NO_VECTOR);
    let mul = caps.notify_off_multiplier;
    let rx = setup_queue(common, Q_RX, RX_MSIX_ENTRY, dma, notify_loc, mul);
    let tx = setup_queue(common, Q_TX, TX_MSIX_ENTRY, dma, notify_loc, mul);
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
    let (mut spawned, mut arp_sent) = (false, false);
    if let (Some(rxq), Some(txq)) = (rx, tx)
        && let Some(ring) = TxRing::new(txq, notify_base, mul, dma)
    {
        crate::cpu::without_interrupts(|| {
            *TX.lock() = Some(ring);
            *SETUP.lock() = Some(Setup {
                notify_base,
                mul,
                rx: rxq,
                mac,
                dma,
            });
        });
        spawned = crate::sched::spawn(target, net_thread, 0).is_ok();
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
                arp_sent = send_arp(Mac(mac), ARP_REQUEST_SENDER, GATEWAY, true);
            }
            crate::sched::yield_until(|| RX_DONE.load(Ordering::Acquire) != 0, tsc_hz, 10);
        }
    }
    let finished = RX_DONE.load(Ordering::Acquire) != 0;
    let gateway = RX_GATEWAY.load(Ordering::Relaxed);
    let lease = RX_LEASE.load(Ordering::Relaxed);
    let blocked = RX_BLOCKED.load(Ordering::Relaxed);
    let wakes = NET_WAKES.load(Ordering::Relaxed);
    let device_faults = DEVICE_FAULTS.load(Ordering::Relaxed);
    let unarmed = RX_ARMED.load(Ordering::Acquire) == 0;
    if !spawned || (finished && (device_faults != 0 || unarmed)) {
        // Nothing drives the device: reset it and take bus mastering away again (I12).
        w8(common + reg::DEVICE_STATUS, 0);
        let _ = crate::pcie::clear_bus_master(at);
    }
    let dhcp_retransmits = DHCP_RETRANSMITS.load(Ordering::Relaxed);
    let irqs = NET_IRQS.load(Ordering::Relaxed);
    let on = NET_IRQ_CPU.load(Ordering::Relaxed).saturating_sub(1);
    let last = if irqs == 0 {
        alloc::string::String::from("none arrived")
    } else {
        match usize::try_from(on).ok().and_then(crate::cpu::CpuIndex::new) {
            Some(c) => alloc::format!("the last on {}", c.named()),
            None => alloc::string::String::from("the last on a processor without a CPU index"),
        }
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
        && on == target.get() as u64
        && device_faults == 0
        && dhcp_retransmits == u64::from(DHCP_INJECT_LOST_DISCOVER);
    let g = gateway.to_be_bytes();
    let dhcp_ms = DHCP_ATTEMPT_NS / 1_000_000;
    let fault_note = if device_faults != 0 {
        "; device fault: a ring's used index ran past the ring"
    } else {
        ""
    };
    let rx_note = alloc::format!(
        "{}{}{}{}{fault_note}",
        timeout_note(waiting::ARP),
        timeout_note(waiting::DHCP),
        timeout_note(waiting::DHCP_REFUSED),
        timeout_note(waiting::NO_ENTROPY)
    );
    let ip = (lease as u32).to_be_bytes();
    let _ = writeln!(
        com1,
        "origin-kernel: virtio-net: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, features_ok {features_ok}, link_up {link_up}, DRIVER_OK {driver_ok}; ARP 10.0.2.2 -> {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}; DHCP lease {}.{}.{}.{} ({dhcp_retransmits} retransmitted after a {dhcp_ms} ms timeout, the first DISCOVER dropped on purpose); receive thread on {}: slept {blocked}, woken by the NIC's interrupt {wakes} (of {irqs} MSI-X interrupts, {last}){rx_note}; consistent {}{}",
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
        target.named(),
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
    let ping_note = timeout_note(waiting::ECHO);
    let ping_ok = finished && gateway != 0 && lease & (1 << 32) != 0 && replies == u64::from(PINGS);
    let _ = writeln!(
        com1,
        "origin-kernel: virtio-net ping: {}.{}.{}.{} -> 10.0.2.2 ({:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}): {replies} of {PINGS} echo replies (identifier, sequence and data matched); round trip min/avg/max {min}/{avg}/{max} us{ping_note}; consistent {}{}",
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
    let retransmits = TFTP_RETRANSMITS.load(Ordering::Relaxed);
    let tftp_ok = finished
        && complete
        && identical
        && retransmits == u64::from(TFTP_INJECT_LOST_REQUEST)
        && bytes == crate::INITRAMFS_IMAGE.len() as u64
        && frames > N_RX as u64
        && recycled > 0;
    let tftp_note = timeout_note(waiting::TFTP);
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
        "origin-kernel: virtio-net TFTP: {TFTP_FILE} from 10.0.2.2: {bytes} bytes in {blocks} blocks{}, {outcome}; {retransmits} retransmitted after a {} ms timeout (the first read request is dropped on purpose); the receive ring's {N_RX} buffers carried {frames} frames ({recycled} recycled){tftp_note}; consistent {}{}",
        if complete && bytes.is_multiple_of(framekernel::tftp::BLOCK as u64) {
            " (the last empty)"
        } else {
            ""
        },
        TFTP_ATTEMPT_NS / 1_000_000,
        tftp_ok,
        if tftp_ok { "" } else { ": virtio-net failed" }
    );

    // The first filesystem content from the network: the fetched file — the downloaded
    // bytes, not the embedded copy — unpacked as an initramfs into a fresh VFS and read back,
    // by the same checks as the embedded image's.
    let fetched = crate::cpu::without_interrupts(|| FETCHED.lock().take());
    let check = crate::check_initramfs(fetched.as_deref().unwrap_or(&[]));
    let unpacked_ok = fetched.is_some() && check.ok;
    let _ = writeln!(
        com1,
        "origin-kernel: virtio-net initramfs: the fetched image extracted into a fresh VFS: {} entries; /etc/hostname {}, implicit /boot dir {}; consistent {}{}",
        check.entries,
        check.hostname,
        check.implicit_dir,
        unpacked_ok,
        if unpacked_ok {
            ""
        } else {
            ": virtio-net failed"
        }
    );
}
