//! The kernel crate's per-processor tables, sized once when the processors are discovered
//! (ADR-0008 Amendment 1, D1; `origin_frame::percpu`).

use origin_frame::percpu::Sizing;

/// This crate's one sizing function: every `PerCpu` table of the kernel crate, sized for the
/// indexes `sizing` asks for. `smp::start` calls it once, after the framework's
/// (`origin_frame::percpu`'s), after the MADT is read and before any application processor is
/// sent INIT. `cargo xtask check` refuses a `PerCpu` static of this crate that it does not name.
pub(crate) fn size_per_cpu(sizing: &mut Sizing) {
    sizing.table(&crate::irq::IOAPIC_IRQS_ON);
    sizing.table(&crate::sched::QUEUES);
    sizing.table(&crate::sched::ACTIVE);
    sizing.table(&crate::sched::THREADS);
    sizing.table(&crate::sched::ZOMBIES);
    sizing.table(&crate::sched::PROGRESS);
    sizing.table(&crate::sched::RAN_ON);
    sizing.table(&crate::storage::IO_WAIT);
    sizing.table(&crate::storage::CPU_INTERRUPTS);
    sizing.table(&crate::storage::CPU_WAKES);
    sizing.table(&crate::storage::CPU_BLOCKS);
    sizing.table(&crate::storage::CPU_QUEUES);
    sizing.table(&crate::storage::CPU_RESULT);
}
