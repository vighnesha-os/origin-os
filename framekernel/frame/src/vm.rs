//! The kernel's address space once it is live, behind a lock, so pages can be mapped or
//! remapped later: firmware tables the kernel reads (ACPI), guard pages and the AP
//! start-up trampoline.
//!
//! Every change that removes access is followed by a TLB shootdown (`tlb`) once the lock is
//! released, so no processor keeps the old translation; changes that only add access need
//! none. Kernel-half mappings are global; the identity pages of the AP trampoline are not.

use core::sync::atomic::{AtomicU64, Ordering};

use origin_paging::{
    Access, AddressSpace, Attributes, LARGE_PAGE, LeafCensus, MapError, PAGE, TablePool,
    Translation,
};

/// The MMIO window (ADR-0004 H2): device registers, mapped uncached on demand. It starts
/// after the 2 GiB reserved for the framebuffer (ADR-0010).
const MMIO_BASE: u64 =
    origin_paging::layout::FRAMEBUFFER_WINDOW + origin_paging::layout::FRAMEBUFFER_WINDOW_SIZE;
const MMIO_END: u64 = 0xFFFF_F000_0000_0000;
/// The next free address in the MMIO window.
static MMIO_NEXT: AtomicU64 = AtomicU64::new(MMIO_BASE);

use origin_boot_info::{MemoryRegion, RegionKind, overlaps_ram};
use origin_sync::SpinLock;

use crate::addr::PhysAddr;
use crate::mmio::MmioRegion;

/// Physical ranges of device registers that have been handed out, at most one owner each. A
/// claim outside `map_mmio` is the framework's own (the IOMMU's registers, ECAM): no driver
/// can get at them through `map_mmio`, because a mapping that overlaps an earlier claim is
/// refused.
struct Claims {
    ranges: [(u64, u64); 64],
    count: usize,
}

impl Claims {
    const fn new() -> Self {
        Self {
            ranges: [(0, 0); 64],
            count: 0,
        }
    }

    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.ranges
            .iter()
            .take(self.count)
            .any(|&(a, b)| start < b && a < end)
    }

    fn add(&mut self, start: u64, end: u64) -> bool {
        match self.ranges.get_mut(self.count) {
            Some(slot) => {
                *slot = (start, end);
                self.count += 1;
                true
            }
            None => false,
        }
    }
}

struct KernelSpace {
    space: AddressSpace,
    tables: TablePool<'static>,
    /// The firmware's memory map: [`map_mmio`] refuses to map anything it calls RAM.
    memory_map: &'static [MemoryRegion],
    /// Device registers handed out so far.
    claims: Claims,
}

static KERNEL_SPACE: SpinLock<Option<KernelSpace>> = SpinLock::new(None);

/// The kernel address space's PML4, for the scheduler's CR3 switches (0 before `install`).
static KERNEL_ROOT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The kernel's PML4 (physical), without taking a lock.
pub fn kernel_root() -> u64 {
    KERNEL_ROOT.load(core::sync::atomic::Ordering::Acquire)
}

/// The kernel half (PML4 entries 256–511) every process address space shares.
pub fn kernel_half() -> Option<[u64; 256]> {
    crate::cpu::without_interrupts(|| {
        let guard = KERNEL_SPACE.lock();
        let k = guard.as_ref()?;
        k.space.upper_half(&k.tables)
    })
}

/// Keeps the live address space, and the firmware's memory map that [`map_mmio`] holds every
/// mapping against (`init_paging` calls this right after loading CR3).
pub fn install(
    space: AddressSpace,
    tables: TablePool<'static>,
    memory_map: &'static [MemoryRegion],
) {
    KERNEL_ROOT.store(space.root(), core::sync::atomic::Ordering::Release);
    crate::cpu::without_interrupts(|| {
        *KERNEL_SPACE.lock() = Some(KernelSpace {
            space,
            tables,
            memory_map,
            claims: Claims::new(),
        });
    })
}

/// Physical address of the live PML4.
pub fn root() -> Option<u64> {
    crate::cpu::without_interrupts(|| KERNEL_SPACE.lock().as_ref().map(|k| k.space.root()))
}

/// Makes physical `[addr, addr + len)` readable at its direct-map address: pages not mapped
/// yet are mapped read-only and never executable; mapped pages keep their mapping.
/// Returns the direct-map address of `addr`.
pub fn map_firmware(addr: u64, len: u64) -> Result<u64, MapError> {
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
pub fn map_user(virt: u64, phys: u64, access: Access) -> Result<(), MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        let attributes = Attributes {
            access,
            user: true,
            global: false,
            cache: origin_paging::Cache::WriteBack,
        };
        k.space.map(&mut k.tables, virt, phys, attributes)
    })
}

/// Removes the leaf at `virt`; once this returns, no processor's TLB holds it.
pub fn unmap(virt: u64) -> Result<u64, MapError> {
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
pub fn make_guard(page: u64) -> Result<(), MapError> {
    unmap_guards(&[page])?;
    crate::tlb::shootdown(page);
    Ok(())
}

/// As [`make_guard`] for several pages, with one full TLB flush on every processor instead
/// of one shootdown per page.
pub fn make_guards(pages: &[u64]) -> Result<(), MapError> {
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
pub enum MmioError {
    /// The firmware's memory map says part of the range is RAM.
    OverlapsRam,
    /// Part of the range was already handed out (see [`map_mmio`]).
    Claimed,
    /// The tables refused the mapping; none of it is left behind.
    Map(MapError),
}

impl From<MapError> for MmioError {
    fn from(e: MapError) -> Self {
        Self::Map(e)
    }
}

/// Maps the device registers at physical `[phys, phys + len)` uncached, read/write and
/// never executable into the MMIO window, and returns them as a typed region. The window keeps
/// the physical address's offset within 2 MiB, so aligned ranges get 2 MiB pages. A range the
/// firmware's memory map calls RAM is refused: writing device registers through an uncached
/// mapping there would corrupt memory, and an [`MmioRegion`] must only ever be device memory.
/// So is a range that overlaps one already handed out: registers have one owner, so the
/// IOMMU's and ECAM's registers, claimed at boot before any driver binds, cannot be reached by a
/// driver asking for them (a device's BAR is the driver's input and can lie). On any failure no
/// leaf remains and the window does not advance, so the next mapping starts clean.
pub fn map_mmio(phys: PhysAddr, len: u64) -> Result<MmioRegion, MmioError> {
    if len == 0 || len > isize::MAX as u64 {
        return Err(MapError::Overflow.into());
    }
    let raw = phys.raw();
    let first = raw & !(PAGE - 1);
    let end = raw
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
        if k.claims.overlaps(first, end) {
            return Err(MmioError::Claimed);
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
        if !k.claims.add(first, end) {
            // The claim table is full: refuse the mapping rather than lose track of it.
            // (A leaf stays behind in the window; no one can reach it, and nothing else
            // will ever be mapped there.)
            return Err(MapError::OutOfMemory.into());
        }
        Ok(start)
    });
    // `map_range` took back what it had installed, and the next mapping will reuse these
    // addresses: no processor may keep a translation of the failed attempt.
    if removed {
        crate::tlb::shootdown_all();
    }
    let virt = mapped? + (raw - first);
    // SAFETY: `[virt, virt + len)` was just mapped uncached, read/write and never executable,
    // to the device registers at `phys` (the memory map says they are not RAM), and the
    // mapping lasts for ever.
    Ok(unsafe { MmioRegion::from_mapped(virt as usize, len as usize, phys) })
}

/// A readable copy of what the firmware put at physical `[addr, addr + len)` (ACPI tables):
/// `&'static` because firmware tables do not change after boot. Refused for anything but the
/// firmware's own data: a range that overlaps memory the kernel owns or will own (`Usable`,
/// the heap, the kernel's image, the loader's memory), which safe code must not be able to
/// read as a `&'static [u8]` while something writes it.
pub fn firmware_bytes(addr: PhysAddr, len: u64) -> Result<&'static [u8], MapError> {
    let raw = addr.raw();
    if raw == 0 || len > isize::MAX as u64 {
        return Err(MapError::Overflow);
    }
    let end = raw.checked_add(len).ok_or(MapError::Overflow)?;
    // Only memory the firmware described as its own (reserved, ACPI, runtime or device
    // memory): never RAM the kernel owns or will own.
    let firmware = [
        RegionKind::Reserved,
        RegionKind::AcpiReclaimable,
        RegionKind::AcpiNvs,
        RegionKind::Mmio,
        RegionKind::RuntimeCode,
        RegionKind::RuntimeData,
    ]
    .map(|kind| kind as u32);
    let is_firmware_data = crate::cpu::without_interrupts(|| {
        KERNEL_SPACE.lock().as_ref().is_some_and(|k| {
            !k.memory_map.iter().any(|r| {
                r.base < end
                    && raw < r.base.saturating_add(r.pages.saturating_mul(PAGE))
                    && !firmware.contains(&r.kind)
            })
        })
    });
    if !is_firmware_data {
        return Err(MapError::OutOfMemory);
    }
    let virt = map_firmware(raw, len)?;
    // SAFETY: `[raw, raw + len)` is firmware data (checked above), mapped readable at its
    // direct-map address `virt` (just ensured), not wrapping (checked), and the firmware does
    // not change its tables after boot; `u8` has no alignment or validity requirement.
    Ok(unsafe {
        core::slice::from_raw_parts(
            core::ptr::with_exposed_provenance::<u8>(virt as usize),
            len as usize,
        )
    })
}

/// The MMIO region describing the framebuffer the kernel's tables mapped (ADR-0010), for the
/// console's pixel writes. Refused if `fb` is not what the loader passed and was placed at
/// `framebuffer_mapping`'s address, so this cannot be made to point at anything else.
pub fn framebuffer_region(fb: &origin_boot_info::Framebuffer) -> Option<MmioRegion> {
    let mapping = origin_paging::layout::framebuffer_mapping(fb)?;
    if mapping.first_pixel != fb.virtual_base {
        return None;
    }
    let bytes = usize::try_from(fb.visible_bytes()).ok()?;
    // Everything the console will draw is mapped: its first page (checked here) and the
    // visible rows (`layout::build` maps the framebuffer's whole visible part, or the
    // console withdraws it before these tables are used).
    let first = translate(fb.virtual_base).filter(|t| t.uncached && t.writable && !t.executable)?;
    if first.phys != fb.base & !(PAGE - 1) {
        return None;
    }
    // SAFETY: the loader mapped the framebuffer uncached at `virtual_base` and `layout::build`
    // keeps that mapping (or the console was withdrawn before these tables loaded, in which
    // case `translate` above found nothing); the memory map says it is not RAM (checked by
    // `console::screen_for`, which constructs the screen before calling this); video memory
    // is device memory, not a Rust object, so 32-bit volatile pixel writes through the
    // region are fine.
    Some(unsafe {
        MmioRegion::from_mapped(fb.virtual_base as usize, bytes, PhysAddr::new(fb.base))
    })
}

/// How the live tables translate `virt`.
pub fn translate(virt: u64) -> Option<Translation> {
    crate::cpu::without_interrupts(|| {
        let guard = KERNEL_SPACE.lock();
        let k = guard.as_ref()?;
        k.space.translate(&k.tables, virt)
    })
}

/// Counts the live tables' leaves by half and global bit (`tlb::prove`).
pub fn leaf_census() -> Option<LeafCensus> {
    crate::cpu::without_interrupts(|| {
        let guard = KERNEL_SPACE.lock();
        let k = guard.as_ref()?;
        Some(k.space.leaf_census(&k.tables))
    })
}

/// Maps a guard page back (read/write, never executable) before its memory is freed.
pub fn unguard(page: u64) -> Result<(), MapError> {
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
pub fn map_identity(page: u64, access: Access) -> Result<(), MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        k.space
            .map(&mut k.tables, page, page, Attributes::kernel(access))
    })
}

/// Whether `virt` is mapped in the live address space.
pub fn is_mapped(virt: u64) -> bool {
    crate::cpu::without_interrupts(|| {
        KERNEL_SPACE
            .lock()
            .as_ref()
            .is_some_and(|k| k.space.translate(&k.tables, virt).is_some())
    })
}
