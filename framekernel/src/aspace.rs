//! Boot proofs of the address spaces (CLAUDE.md §28, §29; ADR-0004 H2; ADR-0013; N14 step 3,
//! S3-5a). The mechanism is the framework's (`origin_frame::userspace`, `origin_frame::vm`).
#![forbid(unsafe_code)]

use core::fmt::Write;

use origin_frame::userspace::UserSpace;
use origin_frame::vm::{self, KernelProbe};

use crate::console::Console;
use crate::cpu::{self, Probe};

/// What reading `addr` does with `space` loaded on this processor. Interrupts stay off from
/// the switch to the switch back: a preemption would load another thread's tables under the
/// probe, and the probe would be testing those.
fn read_from(space: &UserSpace, addr: u64) -> Probe {
    cpu::without_interrupts(|| {
        space.activate();
        let probe = cpu::probe_read(addr);
        vm::activate_kernel_space();
        probe
    })
}

/// ADR-0004 H2: the kernel half of every address space is tables allocated once at boot.
/// - Every process space's PML4 entries 256–511 are the fixed ones, and the kernel's own
///   tables still hold them, whatever has been mapped since the kernel went live.
/// - The test of the defect this rule closes: a space is created first, a kernel mapping is
///   then made in a top-level slot of its own, and the kernel, on that space's tables, reads it.
/// - The control, in every boot: from a space that lacks that slot, which is what every space
///   used to be, the same read faults. So the read can tell the two apart.
pub(crate) fn prove_kernel_half(com1: &mut Console) -> bool {
    let (Some(space), Some(lacking)) = (UserSpace::new(), UserSpace::without_probe_slot()) else {
        let _ = writeln!(
            com1,
            "origin-kernel: address spaces: no memory for the kernel-half proof: address spaces failed"
        );
        return false;
    };
    let Some(probe) = KernelProbe::new() else {
        let _ = writeln!(
            com1,
            "origin-kernel: address spaces: cannot map the probe page: address spaces failed"
        );
        return false;
    };
    let addr = probe.addr();
    let fixed = space.shares_kernel_half() && vm::kernel_half_unchanged();
    let from_space = read_from(&space, addr);
    let from_lacking = read_from(&lacking, addr);
    let visible = from_space == Probe::Completed;
    let control = matches!(from_lacking, Probe::Faulted { error: 0, address } if address == addr);
    let right = fixed && visible && control;
    let _ = writeln!(
        com1,
        "origin-kernel: address spaces: the kernel half is the same tables in every space: {fixed}; a kernel mapping made at {addr:#x}, in a top-level slot of its own, after a space was created is visible from that space: {visible}; from a space lacking the slot, the same read faults: {control}{}",
        if right { "" } else { ": address spaces failed" }
    );
    right
}
