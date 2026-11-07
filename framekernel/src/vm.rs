//! The kernel's address space once it is live, behind a lock, so pages can be mapped or
//! remapped later: firmware tables the kernel reads (ACPI) and the AP start-up trampoline.

use core::arch::asm;

use framekernel::paging::{Access, AddressSpace, Attributes, MapError, PAGE, TablePool};

use crate::sync::SpinLock;

struct KernelSpace {
    space: AddressSpace,
    tables: TablePool<'static>,
}

static KERNEL_SPACE: SpinLock<Option<KernelSpace>> = SpinLock::new(None);

/// Keeps the live address space (`init_paging` calls this right after loading CR3).
pub(crate) fn install(space: AddressSpace, tables: TablePool<'static>) {
    *KERNEL_SPACE.lock() = Some(KernelSpace { space, tables });
}

/// Physical address of the live PML4.
pub(crate) fn root() -> Option<u64> {
    KERNEL_SPACE.lock().as_ref().map(|k| k.space.root())
}

/// Maps, read-only and never executable, every page of `[addr, addr + len)` that is not
/// mapped yet (identity). Pages already mapped keep their mapping.
pub(crate) fn map_readonly(addr: u64, len: u64) -> Result<(), MapError> {
    let mut guard = KERNEL_SPACE.lock();
    let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
    let first = addr & !(PAGE - 1);
    let end = addr.checked_add(len).ok_or(MapError::Overflow)?;
    let mut page = first;
    while page < end {
        if k.space.translate(&k.tables, page).is_none() {
            k.space
                .map(&mut k.tables, page, page, Attributes::kernel(Access::Read))?;
        }
        page += PAGE;
    }
    Ok(())
}

/// Gives the 4 KiB identity page at `page` the access `access`, and flushes its TLB entry.
pub(crate) fn set_access(page: u64, access: Access) -> Result<(), MapError> {
    let mut guard = KERNEL_SPACE.lock();
    let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
    let phys = k.space.unmap(&mut k.tables, page)?;
    let result = k
        .space
        .map(&mut k.tables, page, phys, Attributes::kernel(access));
    // SAFETY: `invlpg` only drops this processor's cached translation for `page`; the next
    // access walks the tables just edited.
    unsafe { asm!("invlpg [{}]", in(reg) page, options(nostack, preserves_flags)) };
    result
}
