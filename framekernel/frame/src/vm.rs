//! The kernel's address space once it is live, behind a lock, so pages can be mapped or
//! remapped later: firmware tables the kernel reads (ACPI), the kernel stacks in their region
//! (`stack`, ADR-0004 Amendment 1, A2), the guard pages of the boot processor's interrupt
//! stacks and the AP start-up trampoline.
//!
//! Every change that removes access is followed by a TLB shootdown (`tlb`) once the lock is
//! released, so no processor keeps the old translation; changes that only add access need
//! none. Kernel-half mappings are global; the identity pages of the AP trampoline are not.
//! Nothing here splits a page of the direct map after boot.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use origin_paging::layout::{DIRECT_MAP_WINDOW, DIRECT_MAP_WINDOW_SIZE, STACK_REGION_SIZE};
use origin_paging::{
    Access, AddressSpace, Attributes, LARGE_PAGE, LeafCensus, LeafSizes, MapError, PAGE, TablePool,
    Translation, kernel_half_is_complete, same_kernel_half,
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
use crate::memory::OwnedFrame;
use crate::mmio::MmioRegion;
use crate::once::SetOnce;

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

/// The kernel half as it was when the kernel's tables went live (`install`). No later mapping
/// changes these entries (`reserve_upper_half`): every address space's copy stays right.
static KERNEL_HALF: SetOnce<[u64; 256]> = SetOnce::new();

/// The kernel half (PML4 entries 256–511) every process address space shares: each points at a
/// table allocated once at boot (ADR-0004 H2). `None` before `install`, and if the half was
/// not complete when it ran.
pub fn kernel_half() -> Option<&'static [u64; 256]> {
    KERNEL_HALF.get()
}

/// The direct map's leaves by size when the kernel's tables went live (`install`).
static DIRECT_MAP_AT_BOOT: SetOnce<LeafSizes> = SetOnce::new();

/// The leaves in the direct-map window of these tables, by size.
fn direct_map_sizes(space: &AddressSpace, tables: &TablePool<'_>) -> LeafSizes {
    space.leaf_sizes(
        tables,
        DIRECT_MAP_WINDOW,
        DIRECT_MAP_WINDOW + DIRECT_MAP_WINDOW_SIZE,
    )
}

/// The direct map's leaves by size, now and when the kernel's tables went live: the direct
/// map's page sizes (ADR-0004 Amendment 1, A2). Firmware tables mapped since (`firmware_bytes`)
/// add 4 KiB leaves of their own; only a split takes a 2 MiB leaf away. `None` before
/// `install`. Walks every table of the window, with interrupts off: for proofs.
pub fn direct_map_leaves() -> Option<(LeafSizes, LeafSizes)> {
    let boot = *DIRECT_MAP_AT_BOOT.get()?;
    let now = crate::cpu::without_interrupts(|| {
        let guard = KERNEL_SPACE.lock();
        let k = guard.as_ref()?;
        Some(direct_map_sizes(&k.space, &k.tables))
    })?;
    Some((now, boot))
}

/// Whether the kernel's own tables still have the entries [`kernel_half`] gave everyone.
pub fn kernel_half_unchanged() -> bool {
    let Some(fixed) = kernel_half() else {
        return false;
    };
    crate::cpu::without_interrupts(|| {
        let guard = KERNEL_SPACE.lock();
        let live = guard.as_ref().and_then(|k| k.space.upper_half(&k.tables));
        live.is_some_and(|live| same_kernel_half(&live, fixed))
    })
}

/// Keeps the live address space, and the firmware's memory map that [`map_mmio`] holds every
/// mapping against (`init_paging` calls this right after loading CR3).
///
/// # Safety
///
/// Once, at boot, with `space` and `tables` the page tables just loaded into CR3: this is the
/// kernel's address space from now on, and every later mapping edits these tables.
pub unsafe fn install(
    space: AddressSpace,
    tables: TablePool<'static>,
    memory_map: &'static [MemoryRegion],
) {
    KERNEL_ROOT.store(space.root(), core::sync::atomic::Ordering::Release);
    if let Some(half) = space.upper_half(&tables)
        && kernel_half_is_complete(&half)
    {
        // Only this call sets it, and `early` makes it once.
        let _ = KERNEL_HALF.set(half);
    }
    // As above: set once, before anything can split a page.
    let _ = DIRECT_MAP_AT_BOOT.set(direct_map_sizes(&space, &tables));
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
fn map_firmware(addr: u64, len: u64) -> Result<u64, MapError> {
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
/// Once this returns, what this processor does next sees the new entries.
///
/// # Safety
///
/// `phys` must be memory that ring 3 may see (the proofs use a fresh page of its own), or
/// every process could read or write it; `virt` must be free in the kernel's tables.
pub unsafe fn map_user(virt: u64, phys: u64, access: Access) -> Result<(), MapError> {
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
    })?;
    // A fetch from the page straight after mapping it found it not present on Intel Xeons
    // under KVM (#PF error 0x10, and the right 0x11 on every later try), in 9 of 10 boots.
    // INVLPG is a serializing instruction (Intel SDM Vol. 3A, "Serializing Instructions"):
    // the entries are in memory before anything after it runs, and no older view of this
    // address survives it.
    crate::cpu::invlpg(virt);
    Ok(())
}

/// Removes the leaf at `virt`; once this returns, no processor's TLB holds it.
///
/// # Safety
///
/// `virt` must be a page nothing relies on being mapped: the kernel's code, its data, a live
/// stack or any Rust object must not lie there. (A mapping this code made is removed through
/// its token instead: [`IdentityMapping::unmap`].)
pub unsafe fn unmap(virt: u64) -> Result<u64, MapError> {
    let phys = crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        k.space.unmap(&mut k.tables, virt)
    })?;
    crate::tlb::shootdown(virt);
    Ok(phys)
}

/// Turns pages of the kernel's image into guard pages: unmapped, so any access faults, on every
/// processor once this returns, with one full TLB flush. A 2 MiB leaf covering one is split
/// first. Only the boot processor's interrupt stacks have guards in the image (`cpu`), since
/// they exist before anything else; every other guard is the unmapped space below a stack in
/// the stack region (`stack`). A page outside the image window is refused, with nothing
/// changed: nothing splits a page of the direct map after boot (ADR-0004 Amendment 1, A2).
pub(crate) fn make_guards(pages: &[u64]) -> Result<(), MapError> {
    // The image window is the top of the address space: it ends where the addresses do.
    use origin_paging::layout::KERNEL_WINDOW;
    if let Some(&outside) = pages.iter().find(|&&page| page < KERNEL_WINDOW) {
        return Err(MapError::NotMapped(outside));
    }
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        for &page in pages {
            k.space.split_large(&mut k.tables, page)?;
            k.space.unmap(&mut k.tables, page)?;
        }
        Ok::<(), MapError>(())
    })?;
    crate::tlb::shootdown_all();
    Ok(())
}

/// Whether the `pages` pages from `virt` lie in the kernel stacks' region, wherever it was
/// placed (`stack::region`).
fn in_stack_region(virt: u64, pages: usize) -> bool {
    let base = crate::stack::region().base;
    let end = (pages as u64)
        .checked_mul(PAGE)
        .and_then(|bytes| virt.checked_add(bytes));
    virt.is_multiple_of(PAGE)
        && virt >= base
        && end.is_some_and(|end| end <= base + STACK_REGION_SIZE)
}

/// Maps `frames` at consecutive pages from `virt` in the kernel stacks' region (`stack`):
/// read/write, never executable, and global like the rest of the kernel half. Each frame it maps
/// is taken out of `frames`: the tables hold it now. Returns how many it mapped, which is all of
/// them unless the tables refused one; the region's tables are made at boot, so only a page
/// that is mapped already can be. Outside the region nothing is mapped.
///
/// No shootdown: a page that was not present is in no TLB, since no processor caches a
/// translation from an entry whose P flag is 0 (Intel SDM Vol. 3A §4.10.4.3).
pub(crate) fn map_stack_pages(virt: u64, frames: &mut [Option<OwnedFrame>]) -> usize {
    if !in_stack_region(virt, frames.len()) {
        return 0;
    }
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let Some(k) = guard.as_mut() else {
            return 0;
        };
        let mut mapped = 0;
        for (page, slot) in (0u64..).zip(frames.iter_mut()) {
            let Some(frame) = slot.as_ref() else {
                break;
            };
            let at = virt + page * PAGE;
            let rw = Attributes::kernel_global(Access::ReadWrite);
            if k.space
                .map(&mut k.tables, at, frame.phys().raw(), rw)
                .is_err()
            {
                break;
            }
            *slot = None;
            mapped += 1;
        }
        mapped
    })
}

/// Takes the stack-region pages from `virt` out of the kernel's tables, one for each entry of
/// `out`, and puts there the frames they held (`None` where nothing was mapped). Their
/// translations may still be cached: the caller shoots down before the addresses or the frames
/// are used again (`origin_sync::slots::Slots::reclaim`). Outside the region nothing changes.
pub(crate) fn unmap_stack_pages(virt: u64, out: &mut [Option<OwnedFrame>]) {
    if !in_stack_region(virt, out.len()) {
        return;
    }
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let Some(k) = guard.as_mut() else {
            return;
        };
        for (page, slot) in (0u64..).zip(out.iter_mut()) {
            *slot = k
                .space
                .unmap(&mut k.tables, virt + page * PAGE)
                .ok()
                .map(|phys| OwnedFrame::from_mapping(PhysAddr::new(phys)));
        }
    });
}

/// Where a [`KernelProbe`] maps its page: PML4 entry 480, just above the MMIO window, which
/// nothing else uses. Exactly the case ADR-0004 H2 is about: a kernel mapping in a top-level
/// slot no earlier mapping has touched.
pub(crate) const PROBE_PAGE: u64 = MMIO_END;
/// [`PROBE_PAGE`]'s index among PML4 entries 256–511.
pub(crate) const PROBE_SLOT: usize = 480 - 256;

/// Whether a [`KernelProbe`] exists (one at a time: there is one probe page).
static PROBE_TAKEN: AtomicBool = AtomicBool::new(false);

/// A kernel page in a top-level slot of its own, for the proof that a mapping made after an
/// address space was created is visible from that space. Unmapped (on every processor) and
/// freed when dropped.
///
/// Unlike the rest of the kernel half it is **not global**: the proof reads it through one space
/// and then through another, and a global translation cached by the first read would survive
/// the CR3 load and answer the second without a walk of that space's tables (review finding 4,
/// seen on an AMD EPYC under KVM; QEMU's software CPU flushes everything on a CR3 load).
#[derive(Debug)]
pub struct KernelProbe {
    frame: Option<OwnedFrame>,
}

impl KernelProbe {
    /// Maps a fresh frame at the probe page, read-only, never executable and not global.
    /// `None` if one exists already, or if the frame or the tables cannot be had.
    pub fn new() -> Option<Self> {
        if PROBE_TAKEN.swap(true, Ordering::AcqRel) {
            return None;
        }
        let mapped = crate::memory::allocate_frame().and_then(|frame| {
            let done = crate::cpu::without_interrupts(|| {
                let mut guard = KERNEL_SPACE.lock();
                let k = guard.as_mut()?;
                k.space
                    .map(
                        &mut k.tables,
                        PROBE_PAGE,
                        frame.phys().raw(),
                        Attributes::kernel(Access::Read),
                    )
                    .ok()
            });
            if done.is_some() {
                Some(frame)
            } else {
                let _ = crate::memory::free_frame(frame);
                None
            }
        });
        if mapped.is_none() {
            PROBE_TAKEN.store(false, Ordering::Release);
        }
        mapped.map(|frame| Self { frame: Some(frame) })
    }

    /// The address of the mapped page.
    #[must_use]
    pub const fn addr(&self) -> u64 {
        PROBE_PAGE
    }

    /// Reads the probe page through a space like every space before ADR-0004 H2: built as
    /// [`UserSpace::new`] builds one, but with the probe page's top-level slot empty, as when a
    /// kernel mapping was made in a slot of its own after the space was created. The in-boot
    /// control of the kernel-half proof, which expects a fault. The space is made and dropped
    /// inside this call, so no space that lacks part of the kernel half ever reaches a caller
    /// (review housekeeping). `None` without memory for it.
    ///
    /// [`UserSpace::new`]: crate::userspace::UserSpace::new
    #[must_use]
    pub fn read_without_its_slot(&self) -> Option<crate::cpu::Probe> {
        let lacking = crate::userspace::UserSpace::without_probe_slot()?;
        Some(lacking.probe_read(self.addr()))
    }
}

impl Drop for KernelProbe {
    fn drop(&mut self) {
        // SAFETY: `PROBE_PAGE` is this probe's own page: nothing but `probe_read` was given
        // its address, and that recovers from a fault, so nothing relies on it being mapped.
        let unmapped = unsafe { unmap(PROBE_PAGE) }.is_ok();
        if unmapped && let Some(frame) = self.frame.take() {
            // `unmap` has returned, so no processor still reaches the frame.
            let _ = crate::memory::free_frame(frame);
        }
        PROBE_TAKEN.store(false, Ordering::Release);
    }
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

/// The firmware framebuffer as a typed region (ADR-0010): the mapping the loader made, which
/// the kernel's own tables keep at the same address (`layout::map_framebuffer`), or `None` if
/// `fb` does not describe it. A forged description can only name addresses in the framebuffer
/// window, never kernel memory, and the memory map must say it is not RAM.
pub fn framebuffer_region(
    fb: &origin_boot_info::Framebuffer,
    regions: &[MemoryRegion],
) -> Option<MmioRegion> {
    let mapping = origin_paging::layout::framebuffer_mapping(fb)?;
    if mapping.first_pixel != fb.virtual_base
        || fb.virtual_base == 0
        || overlaps_ram(regions, fb.base, fb.visible_bytes())
    {
        return None;
    }
    let bytes = usize::try_from(fb.visible_bytes()).ok()?;
    // SAFETY: `first_pixel` is where `layout::map_framebuffer` (and the loader, for the tables
    // in force now) maps the framebuffer, uncached, for its visible bytes; it is not RAM
    // (checked above), so 32-bit volatile pixel writes through the region touch only video
    // memory. A forged `fb` can only produce an address inside the reserved framebuffer
    // window (checked by `framebuffer_mapping`), where nothing else is mapped.
    Some(unsafe {
        MmioRegion::from_mapped(fb.virtual_base as usize, bytes, PhysAddr::new(fb.base))
    })
}

/// Whether `addr` is in a window of device memory (the MMIO window or the framebuffer's): a
/// probe must not read there.
pub(crate) fn in_device_window(addr: u64) -> bool {
    use origin_paging::layout::{FRAMEBUFFER_WINDOW, FRAMEBUFFER_WINDOW_SIZE};
    (FRAMEBUFFER_WINDOW..FRAMEBUFFER_WINDOW + FRAMEBUFFER_WINDOW_SIZE).contains(&addr)
        || (MMIO_BASE..MMIO_END).contains(&addr)
}

/// Switches this processor to the kernel's own address space, if it isn't on it yet: the space
/// of every kernel thread. Only at the scheduler's switch point (`trap::Switch`, review finding
/// 2). Safe: the kernel's tables are complete (`install` keeps them for ever), and this replaces
/// whatever the processor was using, which the switching code (the scheduler) has given up.
pub fn activate_kernel_space(_switch: &crate::trap::Switch) {
    let root = kernel_root();
    if root != 0 {
        // SAFETY: `root` is the kernel's complete PML4, which maps everything kernel code
        // touches next; it lives for ever.
        unsafe { crate::cpu::write_cr3(root) };
    }
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

/// The identity mapping of one low page (the AP trampoline's): removing it is the only thing
/// the holder can do with it.
#[must_use]
#[derive(Debug)]
pub(crate) struct IdentityMapping {
    virt: u64,
}

impl IdentityMapping {
    /// Removes the mapping; once this returns, no processor's TLB holds it.
    pub(crate) fn unmap(self) -> Result<u64, MapError> {
        // SAFETY: `self.virt` is the identity page `map_identity` mapped for this token,
        // used only by the AP trampoline, which has finished: nothing relies on it now.
        unsafe { unmap(self.virt) }
    }
}

/// Maps the 4 KiB page at physical `page` at its identity address (the lower half) with
/// `access`: only for the AP trampoline, which runs there while paging comes on. Safe: a
/// kernel mapping creates no access for safe code, and what it maps is removed through the
/// returned token.
pub(crate) fn map_identity(page: u64, access: Access) -> Result<IdentityMapping, MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        k.space
            .map(&mut k.tables, page, page, Attributes::kernel(access))?;
        Ok(IdentityMapping { virt: page })
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
