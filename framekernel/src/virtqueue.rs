//! The virtio split virtqueue (virtio 1.1 §2.6), the ring transport under every virtio
//! device — the NIC, a block device, the console (CLAUDE.md §31 driver framework, §54
//! networking). A driver shares three arrays with the device: a descriptor table, an
//! "available" ring the driver fills, and a "used" ring the device fills. This module is
//! the driver-side logic and the on-wire encodings, written over plain byte buffers so the
//! kernel drives real DMA memory and the tests drive a `Vec<u8>` with a mock device through
//! the identical code. Pure, no `unsafe`; `alloc` for the descriptor free list.
//!
//! Only the modern layout is modelled: the three rings are independent allocations whose
//! physical addresses the driver programs into the device's common config, so there is no
//! legacy contiguous-alignment arithmetic here.

extern crate alloc;
use alloc::vec::Vec;

/// Descriptor flag: the buffer continues in `next`.
pub const F_NEXT: u16 = 1;
/// Descriptor flag: the device writes this buffer (otherwise the driver wrote it).
pub const F_WRITE: u16 = 2;
/// Descriptor flag: the buffer is a table of indirect descriptors.
pub const F_INDIRECT: u16 = 4;

/// Bytes in one descriptor-table entry.
pub const DESC_SIZE: usize = 16;

/// The byte size of a descriptor table for `queue_size` entries.
pub fn desc_bytes(queue_size: usize) -> usize {
    DESC_SIZE * queue_size
}
/// The byte size of the available ring: `flags`, `idx`, `ring[queue_size]`, `used_event`.
pub fn avail_bytes(queue_size: usize) -> usize {
    6 + 2 * queue_size
}
/// The byte size of the used ring: `flags`, `idx`, `ring[queue_size]` of `(id, len)`,
/// `avail_event`.
pub fn used_bytes(queue_size: usize) -> usize {
    6 + 8 * queue_size
}

fn w16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn w32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn w64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn r16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn r32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn r64(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

/// One descriptor-table entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

impl Descriptor {
    /// Writes this descriptor into entry `i` of a descriptor-table buffer.
    pub fn write(&self, table: &mut [u8], i: usize) {
        let at = i * DESC_SIZE;
        w64(table, at, self.addr);
        w32(table, at + 8, self.len);
        w16(table, at + 12, self.flags);
        w16(table, at + 14, self.next);
    }

    /// Reads entry `i` from a descriptor-table buffer.
    pub fn read(table: &[u8], i: usize) -> Descriptor {
        let at = i * DESC_SIZE;
        Descriptor {
            addr: r64(table, at),
            len: r32(table, at + 8),
            flags: r16(table, at + 12),
            next: r16(table, at + 14),
        }
    }
}

/// A buffer to place in a descriptor chain: a physical address, a length, and whether the
/// device writes it (`device_writable`) or the driver has filled it.
#[derive(Clone, Copy, Debug)]
pub struct Buffer {
    pub addr: u64,
    pub len: u32,
    pub device_writable: bool,
}

/// The driver's pool of free descriptor indices.
#[derive(Debug)]
pub struct FreeList {
    stack: Vec<u16>,
}

impl FreeList {
    /// All `queue_size` descriptors free.
    pub fn new(queue_size: usize) -> Self {
        Self {
            stack: (0..queue_size as u16).rev().collect(),
        }
    }
    pub fn alloc(&mut self) -> Option<u16> {
        self.stack.pop()
    }
    pub fn free(&mut self, index: u16) {
        self.stack.push(index);
    }
    pub fn available(&self) -> usize {
        self.stack.len()
    }
}

/// Allocates a descriptor chain for `buffers` and writes it into `table`; returns the head
/// index, or `None` if the free list cannot supply enough descriptors (the table is left
/// unchanged and nothing is leaked).
pub fn add_chain(free: &mut FreeList, table: &mut [u8], buffers: &[Buffer]) -> Option<u16> {
    if buffers.is_empty() || buffers.len() > free.available() {
        return None;
    }
    let indices: Vec<u16> = (0..buffers.len()).map(|_| free.alloc().unwrap()).collect();
    for (k, buf) in buffers.iter().enumerate() {
        let last = k + 1 == buffers.len();
        let mut flags = 0;
        if buf.device_writable {
            flags |= F_WRITE;
        }
        if !last {
            flags |= F_NEXT;
        }
        Descriptor {
            addr: buf.addr,
            len: buf.len,
            flags,
            next: if last { 0 } else { indices[k + 1] },
        }
        .write(table, indices[k] as usize);
    }
    Some(indices[0])
}

/// Frees a descriptor chain starting at `head`, following `F_NEXT`. Bounded by the queue
/// size, so a corrupt cycle cannot loop forever.
pub fn free_chain(free: &mut FreeList, table: &[u8], queue_size: usize, head: u16) {
    let mut i = head;
    for _ in 0..queue_size {
        let d = Descriptor::read(table, i as usize);
        free.free(i);
        if d.flags & F_NEXT == 0 {
            break;
        }
        i = d.next;
    }
}

// --- Available ring (driver → device) ---

/// The driver's current available index.
pub fn avail_idx(avail: &[u8]) -> u16 {
    r16(avail, 2)
}

/// Publishes descriptor `head` to the available ring and bumps the available index.
pub fn avail_push(avail: &mut [u8], queue_size: usize, head: u16) {
    let idx = avail_idx(avail);
    let slot = (idx as usize) % queue_size;
    w16(avail, 4 + 2 * slot, head);
    w16(avail, 2, idx.wrapping_add(1));
}

/// The `i`-th entry of the available ring (used by a device or a loopback test).
pub fn avail_ring(avail: &[u8], i: usize) -> u16 {
    r16(avail, 4 + 2 * i)
}

// --- Used ring (device → driver) ---

/// The device's current used index.
pub fn used_idx(used: &[u8]) -> u16 {
    r16(used, 2)
}

/// The `i`-th used element: the head descriptor id it completed and the bytes written.
pub fn used_elem(used: &[u8], i: usize) -> (u32, u32) {
    let at = 4 + 8 * i;
    (r32(used, at), r32(used, at + 4))
}

/// Device side (for a loopback/mock device): records that `head` completed with `len` bytes
/// written, into used-ring slot `index`, and bumps the used index.
pub fn used_push(used: &mut [u8], queue_size: usize, index: u16, head: u16, len: u32) {
    let slot = (index as usize) % queue_size;
    let at = 4 + 8 * slot;
    w32(used, at, u32::from(head));
    w32(used, at + 4, len);
    w16(used, 2, index.wrapping_add(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_round_trips() {
        let mut table = alloc::vec![0u8; desc_bytes(4)];
        let d = Descriptor {
            addr: 0xDEAD_BEEF_0000_1000,
            len: 1514,
            flags: F_NEXT | F_WRITE,
            next: 3,
        };
        d.write(&mut table, 2);
        assert_eq!(Descriptor::read(&table, 2), d);
        // The bytes are little-endian at the right offset.
        assert_eq!(&table[32..40], &0xDEAD_BEEF_0000_1000u64.to_le_bytes());
    }

    #[test]
    fn free_list_allocates_and_returns() {
        let mut free = FreeList::new(4);
        assert_eq!(free.available(), 4);
        // Allocation hands out ascending indices (0,1,2,3) from the reversed stack.
        let a = [free.alloc(), free.alloc(), free.alloc(), free.alloc()];
        assert_eq!(a, [Some(0), Some(1), Some(2), Some(3)]);
        assert_eq!(free.alloc(), None, "empty");
        free.free(1);
        assert_eq!(free.alloc(), Some(1));
    }

    #[test]
    fn add_chain_links_buffers_with_the_right_flags() {
        let size = 8;
        let mut free = FreeList::new(size);
        let mut table = alloc::vec![0u8; desc_bytes(size)];
        let head = add_chain(
            &mut free,
            &mut table,
            &[
                Buffer {
                    addr: 0x1000,
                    len: 12,
                    device_writable: false,
                },
                Buffer {
                    addr: 0x2000,
                    len: 1514,
                    device_writable: true,
                },
            ],
        )
        .unwrap();
        let d0 = Descriptor::read(&table, head as usize);
        assert_eq!((d0.addr, d0.len), (0x1000, 12));
        assert_eq!(d0.flags, F_NEXT, "readable, has next");
        let d1 = Descriptor::read(&table, d0.next as usize);
        assert_eq!((d1.addr, d1.len), (0x2000, 1514));
        assert_eq!(d1.flags, F_WRITE, "device-writable, last");
        assert_eq!(free.available(), size - 2);

        // A request larger than the free list is refused without leaking descriptors.
        let mut small = FreeList::new(1);
        let mut t2 = alloc::vec![0u8; desc_bytes(1)];
        assert_eq!(
            add_chain(
                &mut small,
                &mut t2,
                &[
                    Buffer {
                        addr: 0,
                        len: 1,
                        device_writable: false
                    },
                    Buffer {
                        addr: 0,
                        len: 1,
                        device_writable: false
                    },
                ]
            ),
            None
        );
        assert_eq!(small.available(), 1, "nothing consumed on failure");
    }

    /// A full driver↔device round trip through the three rings, over the same byte buffers
    /// the kernel would map as DMA memory.
    #[test]
    fn driver_and_device_exchange_through_the_rings() {
        let size = 8;
        let mut free = FreeList::new(size);
        let mut table = alloc::vec![0u8; desc_bytes(size)];
        let mut avail = alloc::vec![0u8; avail_bytes(size)];
        let mut used = alloc::vec![0u8; used_bytes(size)];

        // Driver posts two requests (a TX-style chain and an RX-style writable buffer).
        let tx = add_chain(
            &mut free,
            &mut table,
            &[Buffer {
                addr: 0x4000,
                len: 60,
                device_writable: false,
            }],
        )
        .unwrap();
        avail_push(&mut avail, size, tx);
        let rx = add_chain(
            &mut free,
            &mut table,
            &[Buffer {
                addr: 0x5000,
                len: 2048,
                device_writable: true,
            }],
        )
        .unwrap();
        avail_push(&mut avail, size, rx);
        assert_eq!(avail_idx(&avail), 2);

        // Mock device: consume both available entries, complete them in the used ring.
        let mut dev_last = 0u16;
        for processed in 0..2u16 {
            let head = avail_ring(&avail, processed as usize);
            let d = Descriptor::read(&table, head as usize);
            // A device-writable buffer reports bytes written; a driver buffer reports 0.
            let written = if d.flags & F_WRITE != 0 { 100 } else { 0 };
            used_push(&mut used, size, dev_last, head, written);
            dev_last = dev_last.wrapping_add(1);
        }
        assert_eq!(used_idx(&used), 2);

        // Driver reaps both and returns their descriptors to the free list.
        let mut driver_last = 0u16;
        while driver_last != used_idx(&used) {
            let (id, len) = used_elem(&used, (driver_last as usize) % size);
            assert!(id == u32::from(tx) || id == u32::from(rx));
            if id == u32::from(rx) {
                assert_eq!(len, 100, "the writable buffer reported bytes");
            }
            free_chain(&mut free, &table, size, id as u16);
            driver_last = driver_last.wrapping_add(1);
        }
        assert_eq!(free.available(), size, "all descriptors returned");
    }
}
