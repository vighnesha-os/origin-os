//! A modern virtio-net driver bring-up (virtio 1.1; CLAUDE.md §31 driver framework, §54
//! networking). It finds the virtio-net PCI function, discovers its capability structures
//! (`virtio::find_caps`), maps the common/notify/device-config regions, runs the status and
//! feature handshake to `DRIVER_OK`, sets up the receive and transmit virtqueues with DMA
//! rings (`virtqueue`), reads the device MAC, and transmits one frame — confirming the
//! device consumed it through the used ring. This is the first NIC bring-up; a full
//! receive/transmit datapath that carries the IP stack is the next step.
//!
//! The ring memory is zeroed DMA pages (`storage::dma_page`): the direct map makes them
//! physically contiguous and gives their physical addresses, and x86 DMA is cache-coherent.
//! The `unsafe` here is volatile MMIO and forming slices over those DMA pages.

use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{Ordering, fence};

use framekernel::pci::Address;
use framekernel::virtio::{self, feature, status};
use framekernel::virtqueue::{self, Buffer, Descriptor};

use crate::console::Console;

const VIRTIO_VENDOR: u16 = 0x1AF4;
const Q_RX: u16 = 0;
const Q_TX: u16 = 1;
/// The MAC the boot sets on the device (QEMU `-device virtio-net-pci,mac=…`).
const EXPECTED_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0xAA, 0xBB, 0xCC];

// Common-configuration register offsets (virtio 1.1 §4.1.4.3).
mod reg {
    pub(super) const DEVICE_FEATURE_SELECT: u64 = 0;
    pub(super) const DEVICE_FEATURE: u64 = 4;
    pub(super) const DRIVER_FEATURE_SELECT: u64 = 8;
    pub(super) const DRIVER_FEATURE: u64 = 12;
    pub(super) const DEVICE_STATUS: u64 = 20;
    pub(super) const QUEUE_SELECT: u64 = 22;
    pub(super) const QUEUE_SIZE: u64 = 24;
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
struct Queue {
    size: usize,
    desc: u64,
    avail: u64,
    used: u64,
    notify_off: u16,
}

/// Maps the BAR holding capability `loc` and returns the virtual address of its structure.
fn map_structure(dev: &crate::pcie::Device, loc: virtio::CfgLocation) -> Option<u64> {
    let bar = dev.bars.get(loc.bar as usize)?.as_ref()?;
    let base = crate::vm::map_mmio(bar.address, bar.size).ok()?;
    Some(base + u64::from(loc.offset))
}

/// Sets up queue `q`: a smaller-of device size and 256 ring in three DMA pages, with the
/// physical addresses programmed and the queue enabled.
fn setup_queue(common: u64, q: u16) -> Option<Queue> {
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
    w16(common + reg::QUEUE_ENABLE, 1);
    Some(Queue {
        size,
        desc: dv,
        avail: av,
        used: uv,
        notify_off,
    })
}

/// Transmits one Ethernet frame on the TX queue and waits for the device to consume it
/// (the used ring to advance). Returns whether it completed.
fn transmit(common: u64, tx: &Queue, notify_base: u64, notify_mul: u32, mac: [u8; 6]) -> bool {
    let _ = common;
    let Some((fv, fp)) = crate::storage::dma_page() else {
        return false;
    };
    // [virtio-net header | Ethernet frame]. A minimum 60-byte frame: broadcast dst, our
    // src, a test ethertype, zero payload. QEMU consumes the TX buffer regardless of where
    // the frame would route.
    let hdr = virtio::NetHeader::plain().encode();
    let buf = page(fv, 4096);
    buf[..hdr.len()].copy_from_slice(&hdr);
    let frame = &mut buf[hdr.len()..hdr.len() + 60];
    frame[0..6].copy_from_slice(&[0xFF; 6]);
    frame[6..12].copy_from_slice(&mac);
    frame[12..14].copy_from_slice(&0x88B5u16.to_be_bytes()); // a local-experimental EtherType
    let total = (hdr.len() + 60) as u32;

    // One read-only descriptor at index 0, published to the available ring.
    Descriptor {
        addr: fp,
        len: total,
        flags: 0,
        next: 0,
    }
    .write(page(tx.desc, virtqueue::desc_bytes(tx.size)), 0);
    let _ = Buffer {
        addr: fp,
        len: total,
        device_writable: false,
    };
    virtqueue::avail_push(page(tx.avail, virtqueue::avail_bytes(tx.size)), tx.size, 0);

    // Make the ring writes visible, then notify the device of the TX queue.
    fence(Ordering::SeqCst);
    w16(
        notify_base + u64::from(tx.notify_off) * u64::from(notify_mul),
        Q_TX,
    );

    // Poll the used ring (volatile, so each read re-fetches) until the device completes it.
    for _ in 0..5_000_000u64 {
        if r16(tx.used + 2) != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Brings up the virtio-net device and reports the result (CLAUDE.md §54).
pub(crate) fn prove(com1: &mut Console) {
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

    // Set up the receive and transmit queues.
    let rx = setup_queue(common, Q_RX);
    let tx = setup_queue(common, Q_TX);
    let queues_ok = rx.is_some() && tx.is_some();

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

    // Transmit one frame and confirm the device consumed it.
    let tx_ok = tx
        .as_ref()
        .is_some_and(|tx| transmit(common, tx, notify_base, caps.notify_off_multiplier, mac));

    let mac_ok = mac == EXPECTED_MAC;
    let consistent = features_ok && queues_ok && driver_ok && mac_ok && link_up && tx_ok;
    let _ = writeln!(
        com1,
        "origin-kernel: virtio-net: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, features_ok {features_ok}, link_up {link_up}, RX+TX queues up {queues_ok}, DRIVER_OK {driver_ok}, TX consumed {tx_ok}; consistent {}{}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        consistent,
        if consistent {
            ""
        } else {
            ": virtio-net failed"
        }
    );
}
