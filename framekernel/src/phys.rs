//! Physical and virtual addresses (ADR-0004): RAM is reached through the direct map at
//! `direct_map_base + physical`, the kernel image at `kernel_virtual_base + rva`.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU64, Ordering};

use origin_boot_info::BootInfo;

static DIRECT: AtomicU64 = AtomicU64::new(0);
static KERNEL_VIRTUAL: AtomicU64 = AtomicU64::new(0);
static KERNEL_PHYSICAL: AtomicU64 = AtomicU64::new(0);

/// Records the loader's placement. Called once, first thing after the boot info is read.
pub(crate) fn init(info: &BootInfo) {
    DIRECT.store(info.direct_map_base, Ordering::Relaxed);
    KERNEL_VIRTUAL.store(info.kernel_virtual_base, Ordering::Relaxed);
    KERNEL_PHYSICAL.store(info.kernel_base, Ordering::Relaxed);
}

/// The direct-map address of physical `phys`.
pub(crate) fn to_virt(phys: u64) -> u64 {
    DIRECT.load(Ordering::Relaxed).wrapping_add(phys)
}

/// The kernel image's virtual base (0 before `init`).
pub(crate) fn kernel_virtual_base() -> u64 {
    KERNEL_VIRTUAL.load(Ordering::Relaxed)
}

/// The physical address behind a direct-map or kernel-image address.
pub(crate) fn to_phys(virt: u64) -> u64 {
    let kernel = KERNEL_VIRTUAL.load(Ordering::Relaxed);
    if virt >= kernel {
        virt - kernel + KERNEL_PHYSICAL.load(Ordering::Relaxed)
    } else {
        virt.wrapping_sub(DIRECT.load(Ordering::Relaxed))
    }
}
