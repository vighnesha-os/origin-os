//! The kernel's address space once it is live, behind a lock, so pages can be mapped or
//! remapped later: firmware tables the kernel reads (ACPI) and the AP start-up trampoline.

use core::arch::asm;

use framekernel::paging::{Access, AddressSpace, Attributes, MapError, PAGE, TablePool};

use origin_sync::SpinLock;

struct KernelSpace {
    space: AddressSpace,
    tables: TablePool<'static>,
}

static KERNEL_SPACE: SpinLock<Option<KernelSpace>> = SpinLock::new(None);

/// Keeps the live address space (`init_paging` calls this right after loading CR3).
pub(crate) fn install(space: AddressSpace, tables: TablePool<'static>) {
    crate::cpu::without_interrupts(|| {
        *KERNEL_SPACE.lock() = Some(KernelSpace { space, tables });
    })
}

/// Physical address of the live PML4.
pub(crate) fn root() -> Option<u64> {
    crate::cpu::without_interrupts(|| KERNEL_SPACE.lock().as_ref().map(|k| k.space.root()))
}

/// Maps, read-only and never executable, every page of `[addr, addr + len)` that is not
/// mapped yet (identity). Pages already mapped keep their mapping.
pub(crate) fn map_readonly(addr: u64, len: u64) -> Result<(), MapError> {
    crate::cpu::without_interrupts(|| {
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
    })
}

/// Gives the 4 KiB identity page at `page` the access `access`, and flushes its TLB entry.
pub(crate) fn set_access(page: u64, access: Access) -> Result<(), MapError> {
    crate::cpu::without_interrupts(|| {
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
        };
        k.space.map(&mut k.tables, virt, phys, attributes)
    })
}

/// Removes the leaf at `virt` and flushes its TLB entry on this processor.
pub(crate) fn unmap(virt: u64) -> Result<u64, MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        let phys = k.space.unmap(&mut k.tables, virt)?;
        // SAFETY: as in `set_access`.
        unsafe { asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
        Ok(phys)
    })
}

/// Turns the identity page at `page` into a guard page: unmapped, so any access faults.
/// A 2 MiB leaf covering it is split first. Other processors must flush their TLBs before
/// relying on the guard (each AP reloads CR3 when its scheduler starts).
pub(crate) fn make_guard(page: u64) -> Result<(), MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        k.space.split_large(&mut k.tables, page)?;
        k.space.unmap(&mut k.tables, page)?;
        drop(guard);
        // SAFETY: reloading CR3 with its own value flushes this processor's non-global TLB
        // entries (the kernel uses no global pages), including the old 2 MiB translation.
        unsafe {
            asm!("mov {tmp}, cr3", "mov cr3, {tmp}", tmp = out(reg) _, options(nostack, preserves_flags));
        }
        Ok(())
    })
}

/// Flushes this processor's TLB (all non-global entries).
pub(crate) fn flush_tlb() {
    // SAFETY: as in `make_guard`.
    unsafe {
        asm!("mov {tmp}, cr3", "mov cr3, {tmp}", tmp = out(reg) _, options(nostack, preserves_flags));
    }
}

/// Maps a guard page back (read/write, never executable) before its memory is freed.
pub(crate) fn unguard(page: u64) -> Result<(), MapError> {
    crate::cpu::without_interrupts(|| {
        let mut guard = KERNEL_SPACE.lock();
        let k = guard.as_mut().ok_or(MapError::OutOfMemory)?;
        k.space.map(
            &mut k.tables,
            page,
            page,
            Attributes::kernel(Access::ReadWrite),
        )
    })
}
