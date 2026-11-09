//! The virtio split virtqueue (virtio 1.1 §2.6), the ring transport under every virtio
//! device — the NIC, a block device, the console (CLAUDE.md §31 driver framework, §54
//! networking). A driver shares three arrays with the device: a descriptor table, an
//! "available" ring the driver fills, and a "used" ring the device fills. This module is
//! the driver-side logic and the on-wire encodings, written over [`RingMemory`] so the
//! kernel drives real DMA memory (through volatile accesses, with no reference covering
//! memory the device can write) and the tests drive a `Vec<u8>` with a mock device through
//! the identical code. Pure, no `unsafe`; `alloc` for the descriptor free list.
//!
//! Only the modern layout is modelled: the three rings are independent allocations whose
//! physical addresses the driver programs into the device's common config, so there is no
//! legacy contiguous-alignment arithmetic here.

extern crate alloc;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

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

/// Memory a ring lives in, addressed by byte offset; every multi-byte field is little-endian.
///
/// The kernel implements this over DMA memory with one volatile load or store of each
/// field's own width, so the device, writing the same memory at any moment, never meets a
/// reference to it and an index is never published or read in halves.
pub trait RingMemory {
    /// How many bytes this memory holds.
    fn byte_len(&self) -> usize;
    fn read_u16(&self, at: usize) -> u16;
    fn read_u32(&self, at: usize) -> u32;
    fn read_u64(&self, at: usize) -> u64;
    fn write_u16(&mut self, at: usize, value: u16);
    fn write_u32(&mut self, at: usize, value: u32);
    fn write_u64(&mut self, at: usize, value: u64);
}

/// The tests drive the rings over plain buffers. Only the tests: the kernel has no path that
/// hands the ring code a slice over memory the device can write.
#[cfg(test)]
impl<T: AsRef<[u8]> + AsMut<[u8]> + ?Sized> RingMemory for T {
    fn byte_len(&self) -> usize {
        self.as_ref().len()
    }
    fn read_u16(&self, at: usize) -> u16 {
        let b = self.as_ref();
        u16::from_le_bytes([b[at], b[at + 1]])
    }
    fn read_u32(&self, at: usize) -> u32 {
        let b = self.as_ref();
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }
    fn read_u64(&self, at: usize) -> u64 {
        let mut v = [0u8; 8];
        v.copy_from_slice(&self.as_ref()[at..at + 8]);
        u64::from_le_bytes(v)
    }
    fn write_u16(&mut self, at: usize, value: u16) {
        self.as_mut()[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn write_u32(&mut self, at: usize, value: u32) {
        self.as_mut()[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn write_u64(&mut self, at: usize, value: u64) {
        self.as_mut()[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
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
    /// Writes this descriptor into entry `i` of a descriptor table.
    pub fn write<M: RingMemory + ?Sized>(&self, table: &mut M, i: usize) {
        let at = i * DESC_SIZE;
        table.write_u64(at, self.addr);
        table.write_u32(at + 8, self.len);
        table.write_u16(at + 12, self.flags);
        table.write_u16(at + 14, self.next);
    }

    /// Reads entry `i` from a descriptor table.
    pub fn read<M: RingMemory + ?Sized>(table: &M, i: usize) -> Descriptor {
        let at = i * DESC_SIZE;
        Descriptor {
            addr: table.read_u64(at),
            len: table.read_u32(at + 8),
            flags: table.read_u16(at + 12),
            next: table.read_u16(at + 14),
        }
    }

    /// Reads entry `i` if the table holds it: for an index the device supplied.
    pub fn get<M: RingMemory + ?Sized>(table: &M, i: usize) -> Option<Descriptor> {
        let end = i.checked_mul(DESC_SIZE)?.checked_add(DESC_SIZE)?;
        (end <= table.byte_len()).then(|| Descriptor::read(table, i))
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
    /// Whether each descriptor is on the stack, so one is never free twice.
    is_free: Vec<bool>,
}

impl FreeList {
    /// All `queue_size` descriptors free.
    pub fn new(queue_size: usize) -> Self {
        Self {
            stack: (0..queue_size as u16).rev().collect(),
            is_free: alloc::vec![true; queue_size],
        }
    }

    pub fn alloc(&mut self) -> Option<u16> {
        let index = self.stack.pop()?;
        if let Some(slot) = self.is_free.get_mut(usize::from(index)) {
            *slot = false;
        }
        Some(index)
    }

    /// Returns `index` to the pool, or returns `false` if it is out of range or already
    /// free: a device reports completed indices, and neither mistake may get in the list.
    pub fn free(&mut self, index: u16) -> bool {
        match self.is_free.get_mut(usize::from(index)) {
            Some(slot) if !*slot => {
                *slot = true;
                self.stack.push(index);
                true
            }
            _ => false,
        }
    }

    pub fn available(&self) -> usize {
        self.stack.len()
    }
}

/// Allocates a descriptor chain for `buffers` and writes it into `table`; returns the head
/// index, or `None` if the free list cannot supply enough descriptors (the table is left
/// unchanged and nothing is leaked).
pub fn add_chain<M: RingMemory + ?Sized>(
    free: &mut FreeList,
    table: &mut M,
    buffers: &[Buffer],
) -> Option<u16> {
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

/// Frees a descriptor chain starting at `head`, following `F_NEXT`, and returns how many
/// descriptors it freed. `head` comes from the used ring and each `next` from memory the
/// device can write: a walk stops at an index outside the table, at a descriptor that is
/// already free, and after `queue_size` steps, so neither a corrupt cycle nor a repeated or
/// out-of-range id can loop or panic.
pub fn free_chain<M: RingMemory + ?Sized>(
    free: &mut FreeList,
    table: &M,
    queue_size: usize,
    head: u16,
) -> usize {
    let mut i = head;
    let mut freed = 0;
    for _ in 0..queue_size {
        let Some(d) = Descriptor::get(table, usize::from(i)) else {
            break;
        };
        if !free.free(i) {
            break;
        }
        freed += 1;
        if d.flags & F_NEXT == 0 {
            break;
        }
        i = d.next;
    }
    freed
}

/// The size to run a split queue at: the device's maximum, at most `limit`, or `None` if the
/// device offers no queue or a size that is not a power of two (virtio 1.1 §2.6: the ring
/// indices wrap at 65536, so anything else breaks the slot arithmetic at the wrap).
pub fn queue_size(device_max: u16, limit: u16) -> Option<usize> {
    device_max
        .is_power_of_two()
        .then(|| usize::from(device_max.min(limit)))
}

// --- Available ring (driver → device) ---

/// The driver's current available index.
pub fn avail_idx<M: RingMemory + ?Sized>(avail: &M) -> u16 {
    avail.read_u16(2)
}

/// Publishes descriptor `head` to the available ring and bumps the available index.
pub fn avail_push<M: RingMemory + ?Sized>(avail: &mut M, queue_size: usize, head: u16) {
    let idx = avail_idx(avail);
    let slot = (idx as usize) % queue_size;
    avail.write_u16(4 + 2 * slot, head);
    // The entry (and the descriptors and buffers written before it) must be visible before
    // the index that publishes it to the device (virtio 1.1 §2.7.13): a compiler and CPU
    // barrier between the two stores.
    fence(Ordering::SeqCst);
    avail.write_u16(2, idx.wrapping_add(1));
}

/// The `i`-th entry of the available ring (used by a device or a loopback test).
pub fn avail_ring<M: RingMemory + ?Sized>(avail: &M, i: usize) -> u16 {
    avail.read_u16(4 + 2 * i)
}

// --- Used ring (device → driver) ---

/// The device's current used index.
pub fn used_idx<M: RingMemory + ?Sized>(used: &M) -> u16 {
    used.read_u16(2)
}

/// The `i`-th used element: the head descriptor id it completed and the bytes written.
pub fn used_elem<M: RingMemory + ?Sized>(used: &M, i: usize) -> (u32, u32) {
    let at = 4 + 8 * i;
    (used.read_u32(at), used.read_u32(at + 4))
}

/// The device claimed something its ring cannot hold: stop trusting it (it needs a reset).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceFault;

/// How many entries the used ring holds past `last_used`, given the index the device
/// published. At most the queue size, or the device is lying: [`DeviceFault`].
pub fn used_advance(published: u16, last_used: u16, queue_size: usize) -> Result<u16, DeviceFault> {
    let advance = published.wrapping_sub(last_used);
    if usize::from(advance) > queue_size {
        return Err(DeviceFault);
    }
    Ok(advance)
}

/// Consumes the used ring's new entries, calling `completed(id, len)` for each in order and
/// moving `last_used` past it; returns how many. The device's index is read once, so this
/// visits at most `queue_size` entries however the device moves the index meanwhile, and a
/// larger advance consumes nothing and is a [`DeviceFault`].
pub fn reap<M: RingMemory + ?Sized>(
    used: &M,
    queue_size: usize,
    last_used: &mut u16,
    mut completed: impl FnMut(u32, u32),
) -> Result<usize, DeviceFault> {
    let advance = used_advance(used_idx(used), *last_used, queue_size)?;
    // The index that published the entries must be read before the entries themselves.
    fence(Ordering::SeqCst);
    for _ in 0..advance {
        let (id, len) = used_elem(used, usize::from(*last_used) % queue_size);
        *last_used = last_used.wrapping_add(1);
        completed(id, len);
    }
    Ok(usize::from(advance))
}

/// Device side (for a loopback/mock device): records that `head` completed with `len` bytes
/// written, into used-ring slot `index`, and bumps the used index.
pub fn used_push<M: RingMemory + ?Sized>(
    used: &mut M,
    queue_size: usize,
    index: u16,
    head: u16,
    len: u32,
) {
    let slot = (index as usize) % queue_size;
    let at = 4 + 8 * slot;
    used.write_u32(at, u32::from(head));
    used.write_u32(at + 4, len);
    used.write_u16(2, index.wrapping_add(1));
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

    /// The ids and links a device reports are checked: an out-of-range head, a repeated
    /// id, a `next` outside the table and a cycle all stop the walk without a panic or a
    /// descriptor freed twice.
    #[test]
    fn a_hostile_device_cannot_corrupt_the_free_list() {
        let size = 8;
        let mut free = FreeList::new(size);
        let table = alloc::vec![0u8; desc_bytes(size)];
        // Out of range: refused, and the free count does not change.
        assert_eq!(free_chain(&mut free, &table, size, 8), 0);
        assert_eq!(free_chain(&mut free, &table, size, u16::MAX), 0);
        assert_eq!(free.available(), size);
        // A descriptor in use is freed exactly once.
        let a = free.alloc().unwrap();
        assert_eq!(free_chain(&mut free, &table, size, a), 1);
        assert_eq!(free_chain(&mut free, &table, size, a), 0, "a repeated id");
        assert_eq!(free.available(), size);
        // `next` leaving the table, and a cycle.
        let (b, c) = (free.alloc().unwrap(), free.alloc().unwrap());
        let mut table = alloc::vec![0u8; desc_bytes(size)];
        let link = |next: u16| Descriptor {
            addr: 0,
            len: 0,
            flags: F_NEXT,
            next,
        };
        link(c).write(&mut table, usize::from(b));
        link(200).write(&mut table, usize::from(c));
        assert_eq!(
            free_chain(&mut free, &table, size, b),
            2,
            "stops at the bad link"
        );
        let (b, c) = (free.alloc().unwrap(), free.alloc().unwrap());
        link(c).write(&mut table, usize::from(b));
        link(b).write(&mut table, usize::from(c));
        assert_eq!(
            free_chain(&mut free, &table, size, b),
            2,
            "a cycle ends the walk"
        );
        assert_eq!(free.available(), size);
        assert!(!free.free(9), "out of range");
    }

    /// A used ring whose device moves `used.idx` on every read: the driver must not keep
    /// chasing it. The reads are counted, and bounded so a chasing driver fails the test
    /// instead of hanging it.
    struct RunawayUsed {
        idx: core::cell::Cell<u16>,
        idx_reads: core::cell::Cell<u32>,
    }

    impl RingMemory for RunawayUsed {
        fn byte_len(&self) -> usize {
            used_bytes(8)
        }
        fn read_u16(&self, at: usize) -> u16 {
            assert_eq!(
                at, 2,
                "the only u16 of the used ring the driver reads is idx"
            );
            self.idx_reads.set(self.idx_reads.get() + 1);
            assert!(
                self.idx_reads.get() < 1000,
                "the driver is chasing used.idx"
            );
            // Each read shows one more completion than the driver has seen so far.
            self.idx.set(self.idx.get().wrapping_add(1));
            self.idx.get()
        }
        fn read_u32(&self, _at: usize) -> u32 {
            0
        }
        fn read_u64(&self, _at: usize) -> u64 {
            0
        }
        fn write_u16(&mut self, _at: usize, _value: u16) {}
        fn write_u32(&mut self, _at: usize, _value: u32) {}
        fn write_u64(&mut self, _at: usize, _value: u64) {}
    }

    #[test]
    fn reaping_reads_used_idx_once_and_stops() {
        let used = RunawayUsed {
            idx: core::cell::Cell::new(0),
            idx_reads: core::cell::Cell::new(0),
        };
        let (mut last, mut seen) = (0u16, 0u32);
        assert_eq!(reap(&used, 8, &mut last, |_, _| seen += 1), Ok(1));
        assert_eq!((last, seen), (1, 1), "one read, one completion");
        assert_eq!(used.idx_reads.get(), 1, "the index is read once per reap");
    }

    /// A device can complete at most a ring's worth of buffers: exactly that many are
    /// consumed; one more is a device fault that consumes nothing.
    #[test]
    fn a_used_index_past_the_queue_size_is_a_device_fault() {
        let size = 8;
        let mut used = alloc::vec![0u8; used_bytes(size)];

        // A full ring is legal, and consumed in order with the ids and lengths reported.
        for i in 0..size as u16 {
            used_push(&mut used, size, i, i + 100, u32::from(i) + 7);
        }
        let mut last = 0u16;
        let mut seen = Vec::new();
        assert_eq!(
            reap(&used, size, &mut last, |id, len| seen.push((id, len))),
            Ok(size)
        );
        assert_eq!(last, size as u16);
        assert_eq!(seen.len(), size);
        assert_eq!(seen[3], (103, 10));
        assert_eq!(
            reap(&used, size, &mut last, |_, _| ()),
            Ok(0),
            "nothing new"
        );

        // The index wraps at 65536 without being a fault.
        used[2..4].copy_from_slice(&2u16.to_le_bytes());
        let mut last = u16::MAX - 1;
        assert_eq!(reap(&used, size, &mut last, |_, _| ()), Ok(4));
        assert_eq!(last, 2);

        // One entry more than the ring holds, at the wrap and away from it, and the runaway
        // value a hostile device would pick: a fault, and `last_used` stays put.
        for (last_used, idx) in [
            (0u16, size as u16 + 1),
            (u16::MAX - 3, size as u16 - 3),
            (10, 9), // the device moved the index backwards
        ] {
            used[2..4].copy_from_slice(&idx.to_le_bytes());
            let mut last = last_used;
            let mut seen = 0;
            assert_eq!(
                reap(&used, size, &mut last, |_, _| seen += 1),
                Err(DeviceFault),
                "{last_used} -> {idx}"
            );
            assert_eq!((last, seen), (last_used, 0), "{last_used} -> {idx}");
        }

        assert_eq!(used_advance(5, 5, 8), Ok(0));
        assert_eq!(used_advance(13, 5, 8), Ok(8));
        assert_eq!(used_advance(14, 5, 8), Err(DeviceFault));
    }

    /// The field accesses go through [`RingMemory`] with the widths and offsets the layout
    /// gives them: the kernel's volatile implementation relies on these being its only
    /// accesses, each aligned to its own width.
    #[test]
    fn ring_fields_are_accessed_at_their_own_width_and_alignment() {
        #[derive(Default)]
        struct Counting {
            bad: core::cell::Cell<u32>,
            fields: core::cell::Cell<u32>,
        }
        impl Counting {
            fn check(&self, at: usize, width: usize) {
                self.fields.set(self.fields.get() + 1);
                if !at.is_multiple_of(width) || at + width > 4096 {
                    self.bad.set(self.bad.get() + 1);
                }
            }
        }
        impl RingMemory for Counting {
            fn byte_len(&self) -> usize {
                4096
            }
            fn read_u16(&self, at: usize) -> u16 {
                self.check(at, 2);
                0
            }
            fn read_u32(&self, at: usize) -> u32 {
                self.check(at, 4);
                0
            }
            fn read_u64(&self, at: usize) -> u64 {
                self.check(at, 8);
                0
            }
            fn write_u16(&mut self, at: usize, _value: u16) {
                self.check(at, 2);
            }
            fn write_u32(&mut self, at: usize, _value: u32) {
                self.check(at, 4);
            }
            fn write_u64(&mut self, at: usize, _value: u64) {
                self.check(at, 8);
            }
        }

        let size = 256; // the largest queue the driver runs; every field is in one page
        let mut mem = Counting::default();
        for i in [0usize, 1, 255] {
            Descriptor {
                addr: 1,
                len: 2,
                flags: 3,
                next: 4,
            }
            .write(&mut mem, i);
            let _ = Descriptor::read(&mem, i);
            let _ = avail_ring(&mem, i);
            let _ = used_elem(&mem, i);
            used_push(&mut mem, size, i as u16, 0, 0);
            avail_push(&mut mem, size, i as u16);
        }
        let _ = (avail_idx(&mem), used_idx(&mem));
        assert!(desc_bytes(size) <= 4096 && used_bytes(size) <= 4096);
        assert!(mem.fields.get() > 30);
        assert_eq!(
            mem.bad.get(),
            0,
            "an access that is misaligned or leaves the page"
        );
    }

    #[test]
    fn queue_sizes_must_be_powers_of_two() {
        assert_eq!(queue_size(256, 256), Some(256));
        assert_eq!(queue_size(1024, 256), Some(256), "the driver takes fewer");
        assert_eq!(queue_size(128, 256), Some(128));
        assert_eq!(queue_size(1, 256), Some(1));
        assert_eq!(queue_size(0, 256), None, "no queue");
        assert_eq!(queue_size(3, 256), None, "not a power of two");
        assert_eq!(queue_size(1000, 256), None);
    }

    /// A device that scribbles over the rings it shares never breaks the driver's side: no
    /// panic, no index out of range or free twice, and every descriptor is accounted for
    /// (CLAUDE.md §72).
    #[test]
    fn hostile_rings_never_break_the_driver() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x5251_4E47);
        let mut reclaimed = 0u64;
        for case in 0..crate::testing::cases(1500) {
            let size = 1usize << (1 + rng.below(6));
            let mut free = FreeList::new(size);
            let mut table = alloc::vec![0u8; desc_bytes(size)];
            let mut avail = alloc::vec![0u8; avail_bytes(size)];
            let mut used = alloc::vec![0u8; used_bytes(size)];
            let mut in_flight = alloc::vec![false; size];
            let mut held = 0usize;
            let mut last_used = 0u16;
            for step in 0..80 {
                match rng.below(5) {
                    0 | 1 => {
                        let n = 1 + rng.below(3);
                        let buffers: Vec<Buffer> = (0..n)
                            .map(|_| Buffer {
                                addr: rng.edge_u64(),
                                len: rng.edge_u32(),
                                device_writable: rng.below(2) == 0,
                            })
                            .collect();
                        if let Some(head) = add_chain(&mut free, &mut table, &buffers) {
                            assert!(usize::from(head) < size, "case {case}.{step}");
                            held += n;
                            avail_push(&mut avail, size, head);
                            // The chain's descriptors are the ones the driver wrote.
                            let mut i = head;
                            for (k, buffer) in buffers.iter().enumerate() {
                                let d = Descriptor::read(&table, usize::from(i));
                                assert_eq!(d.addr, buffer.addr, "case {case}.{step}");
                                in_flight[usize::from(i)] = true;
                                if d.flags & F_NEXT == 0 {
                                    assert_eq!(k + 1, n, "case {case}.{step}");
                                }
                                i = d.next;
                            }
                        }
                    }
                    2 => {
                        // The device overwrites part of its rings at will.
                        let (buf, at) = if rng.below(2) == 0 {
                            let at = rng.below(table.len());
                            (&mut table, at)
                        } else {
                            let at = rng.below(used.len());
                            (&mut used, at)
                        };
                        buf[at] = rng.byte();
                    }
                    _ => {
                        // The driver reaps what the device claims; a bad id is the device's.
                        let idx = used_idx(&used);
                        if idx == last_used {
                            let index = last_used;
                            let head = if rng.below(4) == 0 {
                                rng.edge_u32() as u16
                            } else {
                                rng.below(size + 2) as u16
                            };
                            used_push(&mut used, size, index, head, rng.edge_u32());
                        }
                        let slot = usize::from(last_used) % size;
                        let (id, _len) = used_elem(&used, slot);
                        last_used = last_used.wrapping_add(1);
                        let before = free.available();
                        let freed = free_chain(&mut free, &table, size, id as u16);
                        assert_eq!(free.available(), before + freed, "case {case}.{step}");
                        assert!(free.available() <= size, "case {case}.{step}");
                        reclaimed += freed as u64;
                        held = held.saturating_sub(freed);
                    }
                }
                let _ = avail_idx(&avail);
                let _ = avail_ring(&avail, rng.below(size));
                assert!(free.available() <= size, "case {case}.{step}");
                // Nothing the driver can pop is out of range or popped twice.
                let mut drained = free.stack.clone();
                drained.sort_unstable();
                drained.dedup();
                assert_eq!(
                    drained.len(),
                    free.available(),
                    "case {case}.{step}: a repeat"
                );
                assert!(
                    drained.iter().all(|&i| usize::from(i) < size),
                    "case {case}.{step}"
                );
            }
            let _ = held;
        }
        assert!(reclaimed > 1000, "{reclaimed}");
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
