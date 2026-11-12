//! Physical and virtual addresses (ADR-0004): RAM is reached through the direct map at
//! `direct_map_base + physical`, the kernel image at `kernel_virtual_base + rva`.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU64, Ordering};

use origin_boot_info::BootInfo;

static DIRECT: AtomicU64 = AtomicU64::new(0);
static KERNEL_VIRTUAL: AtomicU64 = AtomicU64::new(0);
static KERNEL_PHYSICAL: AtomicU64 = AtomicU64::new(0);
/// Whether [`init`] has run.
static PHYS_SET: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Records the loader's placement. Only `early` calls this, first thing after the boot info is
/// read; a later call changes nothing (the direct map's base is fixed for the kernel's lifetime).
pub(crate) fn init(info: &BootInfo) {
    if PHYS_SET.swap(true, Ordering::AcqRel) {
        return;
    }
    DIRECT.store(info.direct_map_base, Ordering::Relaxed);
    KERNEL_VIRTUAL.store(info.kernel_virtual_base, Ordering::Relaxed);
    KERNEL_PHYSICAL.store(info.kernel_base, Ordering::Relaxed);
}

/// The direct-map address of physical `phys`.
pub fn to_virt(phys: u64) -> u64 {
    DIRECT.load(Ordering::Relaxed).wrapping_add(phys)
}

/// The kernel image's virtual base (0 before `init`).
pub fn kernel_virtual_base() -> u64 {
    KERNEL_VIRTUAL.load(Ordering::Relaxed)
}

/// How a crash report prints `addr` (ADR-0004 Amendment 2, A6.2): as its offset from the base
/// of the sliding window it is in (the image, the kernel stacks' region, the direct map), or as
/// itself when it is in none; never a base, in any mode. Reads only atomics, so an exception
/// handler may call it whatever lock the interrupted code holds.
pub fn told(addr: u64) -> crate::told::Shown {
    crate::told::Shown(origin_paging::layout::tell(
        addr,
        &origin_paging::layout::Bases {
            image: kernel_virtual_base(),
            direct_map: DIRECT.load(Ordering::Relaxed),
            stack_region: crate::stack::region().base,
        },
    ))
}

/// The physical address behind a direct-map or kernel-image address.
pub fn to_phys(virt: u64) -> u64 {
    let kernel = KERNEL_VIRTUAL.load(Ordering::Relaxed);
    if virt >= kernel {
        virt - kernel + KERNEL_PHYSICAL.load(Ordering::Relaxed)
    } else {
        virt.wrapping_sub(DIRECT.load(Ordering::Relaxed))
    }
}
