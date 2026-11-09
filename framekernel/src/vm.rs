//! The kernel's address space once it is live, behind a lock, so pages can be mapped or
//! remapped later: firmware tables the kernel reads (ACPI), guard pages and the AP
//! start-up trampoline.
//!
//! Every change that removes access is followed by a TLB shootdown (`tlb`) once the lock is
//! released, so no processor keeps the old translation; changes that only add access need
//! none. Kernel-half mappings are global; the identity pages of the AP trampoline are not.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU64, Ordering};

use framekernel::paging::{
    Access, AddressSpace, Attributes, LARGE_PAGE, LeafCensus, MapError, PAGE, TablePool,
    Translation,
};

/// The MMIO window (ADR-0004 H2): device registers, mapped uncached on demand. It starts
/// after the 2 GiB reserved for the framebuffer (ADR-0010).
const MMIO_BASE: u64 = framekernel::paging::layout::FRAMEBUFFER_WINDOW
    + framekernel::paging::layout::FRAMEBUFFER_WINDOW_SIZE;
const MMIO_END: u64 = 0xFFFF_F000_0000_0000;
/// The next free address in the MMIO window.
static MMIO_NEXT: AtomicU64 = AtomicU64::new(MMIO_BASE);

use origin_boot_info::{MemoryRegion, overlaps_ram};
use origin_frame::sync::SpinLock;

struct KernelSpace {
    space: AddressSpace,
    tables: TablePool<'static>,
    /// The firmware's memory map, once [`set_memory_map`] has recorded it: [`map_mmio`]
    /// refuses to map anything it calls RAM.
    memory_map: &'static [MemoryRegion],
}

static KERNEL_SPACE: SpinLock<Option<KernelSpace>> = SpinLock::new(None);

/// The kernel address space's PML4, for the scheduler's CR3 switches (0 before `install`).
static KERNEL_ROOT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The kernel's PML4 (physical), without taking a lock.
pub(crate) fn kernel_root() -> u64 {
    KERNEL_ROOT.load(core::sync::atomic::Ordering::Acquire)
}

/// The kernel half (PML4 entries 256–511) every process address space shares.
pub(crate) fn kernel_half() -> Option<[u64; 256]> {
    crate::cpu::without_interrupts(|| {
        let guard = KERNEL_SPACE.lock();
        let k = guard.as_ref()?;
        k.space.upper_half(&k.tables)
    })
}

/// Keeps the live address space (`init_paging` calls this right after loading CR3).
pub(crate) fn install(space: AddressSpace, tables: TablePool<'static>) {
    KERNEL_ROOT.store(space.root(), core::sync::atomic::Ordering::Release);
    crate::cpu::without_interrupts(|| {
        *KERNEL_SPACE.lock() = Some(KernelSpace {
            space,
            tables,
            memory_map: &[],
        });
    })
}

/// Records the firmware's memory map for [`map_mmio`], as soon as the first caller that needs
/// the check runs (`pcie::quiesce`); the console's UART, mapped earlier, checks for itself.
pub(crate) fn set_memory_map(regions: &'static [MemoryRegion]) {
    crate::cpu::without_interrupts(|| {
        if let Some(k) = KERNEL_SPACE.lock().as_mut() {
            k.memory_map = regions;
        }
    });
}

/// Physical address of the live PML4.
pub(crate) fn root() -> Option<u64> {
    crate::cpu::without_interrupts(|| KERNEL_SPACE.lock().as_ref().map(|k| k.space.root()))
}

/// Makes physical `[addr, addr + len)` readable at its direct-map address: pages not mapped
/// yet are mapped read-only and never executable; mapped pages keep their mapping.
/// Returns the direct-map address of `addr`.
pub(crate) fn map_firmware(addr: u64, len: u64) -> Result<u64, MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        let first = addr & !(PAGE - 1);
        let end = addr.checked_add(len).ok_or(MapError::Overflow)?;
        let mut page = first;
        while page < end {
            let virt = crate::phys::to_virt(page);
            if k.space.translate(&k.tables, virt).is_none() {
                k.space.map(
                    &mut k.tables,
                    virt,
                    page,
                    Attributes::kernel_global(Access::Read),
                )?;
            }
            page += PAGE;
        }
        Ok(crate::phys::to_virt(addr))
    })
}

/// Maps the 4 KiB page `virt` to `phys` as a **user** page with `access`: only for probes
/// that prove supervisor protections (SMEP, SMAP); the kernel has no user mappings otherwise.
pub(crate) fn map_user(virt: u64, phys: u64, access: Access) -> Result<(), MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        let attributes = Attributes {
            access,
            user: true,
            global: false,
            cache: framekernel::paging::Cache::WriteBack,
        };
        k.space.map(&mut k.tables, virt, phys, attributes)
    })
}

/// Removes the leaf at `virt`; once this returns, no processor's TLB holds it.
pub(crate) fn unmap(virt: u64) -> Result<u64, MapError> {
    let phys = crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        k.space.unmap(&mut k.tables, virt)
    })?;
    crate::tlb::shootdown(virt);
    Ok(phys)
}

/// Turns the direct-map page `page` into a guard page: unmapped, so any access faults,
/// on every processor once this returns. A 2 MiB leaf covering it is split first (one
/// INVLPG in the page drops the old 2 MiB translation too).
pub(crate) fn make_guard(page: u64) -> Result<(), MapError> {
    unmap_guards(&[page])?;
    crate::tlb::shootdown(page);
    Ok(())
}

/// As [`make_guard`] for several pages, with one full TLB flush on every processor instead
/// of one shootdown per page.
pub(crate) fn make_guards(pages: &[u64]) -> Result<(), MapError> {
    unmap_guards(pages)?;
    crate::tlb::shootdown_all();
    Ok(())
}

/// Splits and unmaps each page under the lock; the caller shoots down.
fn unmap_guards(pages: &[u64]) -> Result<(), MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        for &page in pages {
            k.space.split_large(&mut k.tables, page)?;
            k.space.unmap(&mut k.tables, page)?;
        }
        Ok(())
    })
}

/// Why [`map_mmio`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MmioError {
    /// The firmware's memory map says part of the range is RAM.
    OverlapsRam,
    /// The tables refused the mapping; none of it is left behind.
    Map(MapError),
}

impl From<MapError> for MmioError {
    fn from(e: MapError) -> Self {
        Self::Map(e)
    }
}

/// Maps the device registers at physical `[phys, phys + len)` uncached, read/write and
/// never executable into the MMIO window, and returns the virtual address of `phys`. The
/// window keeps the physical address's offset within 2 MiB, so aligned ranges get 2 MiB
/// pages. A range the firmware's memory map calls RAM is refused: writing device registers
/// through an uncached mapping there would corrupt memory. On any failure no leaf remains
/// and the window does not advance, so the next mapping starts clean.
pub(crate) fn map_mmio(phys: u64, len: u64) -> Result<u64, MmioError> {
    let first = phys & !(PAGE - 1);
    let end = phys
        .checked_add(len)
        .and_then(|e| e.checked_next_multiple_of(PAGE))
        .ok_or(MapError::Overflow)?;
    let bytes = end - first;
    let mut removed = false;
    let mapped = crate::cpu::without_interrupts(|| -> Result<u64, MmioError> {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        if overlaps_ram(k.memory_map, first, bytes) {
            return Err(MmioError::OverlapsRam);
        }
        let start = MMIO_NEXT
            .load(Ordering::Relaxed)
            .checked_next_multiple_of(LARGE_PAGE)
            .and_then(|v| v.checked_add(first % LARGE_PAGE))
            .ok_or(MapError::Overflow)?;
        let stop = start.checked_add(bytes).ok_or(MapError::Overflow)?;
        if stop > MMIO_END {
            return Err(MapError::OutOfMemory.into());
        }
        if let Err(e) = k
            .space
            .map_range(&mut k.tables, start, first, bytes, Attributes::mmio())
        {
            removed = true;
            return Err(e.into());
        }
        MMIO_NEXT.store(stop, Ordering::Relaxed);
        Ok(start)
    });
    // `map_range` took back what it had installed, and the next mapping will reuse these
    // addresses: no processor may keep a translation of the failed attempt.
    if removed {
        crate::tlb::shootdown_all();
    }
    Ok(mapped? + (phys - first))
}

/// How the live tables translate `virt`.
pub(crate) fn translate(virt: u64) -> Option<Translation> {
    crate::cpu::without_interrupts(|| {
        let guard = KERNEL_SPACE.lock();
        let k = guard.as_ref()?;
        k.space.translate(&k.tables, virt)
    })
}

/// Counts the live tables' leaves by half and global bit (`tlb::prove`).
pub(crate) fn leaf_census() -> Option<LeafCensus> {
    crate::cpu::without_interrupts(|| {
        let guard = KERNEL_SPACE.lock();
        let k = guard.as_ref()?;
        Some(k.space.leaf_census(&k.tables))
    })
}

/// Maps a guard page back (read/write, never executable) before its memory is freed.
pub(crate) fn unguard(page: u64) -> Result<(), MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        k.space.map(
            &mut k.tables,
            page,
            crate::phys::to_phys(page),
            Attributes::kernel_global(Access::ReadWrite),
        )
    })
}

/// Maps the 4 KiB page at physical `page` at its identity address (the lower half) with
/// `access`: only for the AP trampoline, which runs there while paging comes on.
pub(crate) fn map_identity(page: u64, access: Access) -> Result<(), MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        k.space
            .map(&mut k.tables, page, page, Attributes::kernel(access))
    })
}

/// Whether `virt` is mapped in the live address space.
pub(crate) fn is_mapped(virt: u64) -> bool {
    crate::cpu::without_interrupts(|| {
        KERNEL_SPACE
            .lock()
            .as_ref()
            .is_some_and(|k| k.space.translate(&k.tables, virt).is_some())
    })
}
