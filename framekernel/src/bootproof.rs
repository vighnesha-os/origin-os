//! What a failed boot proof means for the boot (ADR-0013).
//!
//! Every proof the kernel runs at boot has a row in the table below, which says what its
//! failure means on a real machine:
//!
//! - [`Consequence::Refuse`]: the kernel stops. Nothing can run without what the proof covers,
//!   or it covers a P0 property and a correct kernel cannot fail it through timing, load or a
//!   difference between machines.
//! - [`Consequence::Withdraw`]: the boot goes on without one capability, which is withheld at
//!   the point where it would be used (`DeviceDma::attach`, for instance).
//! - [`Consequence::Report`]: the boot goes on and says so. Nothing relies on the result yet,
//!   or the proof can fail on a correct kernel.
//!
//! The table is the policy, and a test pins it: changing a row means changing the ADR.
//! `kernel_main` records each result in a [`Tally`], stops when the tally says to, and prints
//! the tally's verdict before it halts.

use core::fmt::{self, Write};

use crate::report::PREFIX;

/// What failing a proof means for the rest of the boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consequence {
    /// The kernel stops: the proof covers something it must not run without.
    Refuse,
    /// The boot goes on without a capability; the text says what is gone.
    Withdraw(&'static str),
    /// The boot goes on; nothing relies on the proof's result.
    Report,
}

use Consequence::{Refuse, Report, Withdraw};

/// Generates [`Proof`] from one table, so a new proof cannot be added without saying what its
/// failure means, and [`Proof::ALL`] cannot leave one out.
macro_rules! proofs {
    ($($variant:ident $name:literal => $consequence:expr;)+) => {
        /// The proofs a boot runs, in the order it runs them.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Proof {
            $($variant,)+
        }

        impl Proof {
            /// Every proof, in boot order.
            pub const ALL: &'static [Proof] = &[$(Proof::$variant,)+];

            /// The proof's name, as the boot's last lines give it.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Proof::$variant => $name,)+
                }
            }

            /// What its failure means for the rest of the boot (ADR-0013).
            #[must_use]
            pub const fn consequence(self) -> Consequence {
                match self {
                    $(Proof::$variant => $consequence,)+
                }
            }
        }
    };
}

proofs! {
    // The kernel's own memory: everything after them runs on these.
    Backtrace "frame-pointer backtrace" => Report;
    PageTables "kernel page tables" => Refuse;
    FrameAllocator "frame allocator" => Refuse;
    BusMasterQuiesce "bus-master quiesce" => Withdraw("no device drivers");
    Heap "kernel heap" => Refuse;
    // The time base and interrupts.
    ApicTimer "APIC timer" => Refuse;
    BusMastersSettled "bus transactions settled" => Report;
    NmiDelivery "NMI delivery" => Report;
    NmiStack "NMI stack" => Refuse;
    Clocks "clock sources" => Report;
    IoApic "I/O APIC routing" => Report;
    // Processors and what the firmware says about them.
    Iommu "IOMMU bring-up" => Withdraw("no device drivers");
    Processors "application processors" => Withdraw("fewer processors");
    CpuState "per-processor control state" => Refuse;
    BootSteps "boot steps run once" => Refuse;
    Topology "CPU topology" => Report;
    Numa "NUMA affinities" => Report;
    NumaDistances "NUMA distances" => Report;
    Entropy "hardware entropy" => Report;
    // Threads, and what runs in ring 3.
    Preemption "preemption" => Refuse;
    Fairness "scheduler fairness" => Report;
    Weights "scheduler weights" => Report;
    SimdState "SIMD state across switches" => Refuse;
    ThreadLifecycle "thread lifecycle" => Report;
    SpawnRefusal "refused spawns" => Report;
    UserIsolation "ring-3 isolation" => Refuse;
    TscConsistency "TSC consistency" => Report;
    ClockMonotonic "monotonic clock" => Report;
    ConsoleLines "concurrent console" => Report;
    Tlb "TLB shootdown" => Refuse;
    WaitQueues "wait queues" => Refuse;
    WaitTiming "wait timing" => Report;
    Ipc "IPC and handles" => Refuse;
    Objects "process and thread objects" => Refuse;
    ThreadsReaped "thread reaping" => Report;
    // Devices.
    Pcie "PCIe enumeration" => Withdraw("no device drivers");
    BusMastersQuiet "bus masters quiet" => Withdraw("no device drivers");
    Nvme "NVMe" => Withdraw("no storage");
    IommuDevices "IOMMU proofs through NVMe" => Report;
    IoApicRemapped "remapped I/O APIC" => Report;
    BusMastering "bus mastering after the drivers" => Report;
    // Libraries, on the heap.
    Vfs "VFS" => Report;
    Initramfs "initramfs" => Report;
    Raster "rasterizer" => Report;
    I8042 "i8042" => Report;
    Net "network packets" => Report;
    Tcp "TCP" => Report;
    TimerQueue "timer queue" => Report;
    Sysfs "/sys" => Report;
    ConsoleUartSections "console UART sections" => Report;
    ConsoleSections "console screen sections" => Report;
}

// A proof's bit in the tally's two words.
const _: () = assert!(Proof::ALL.len() <= u64::BITS as usize);

/// Which proofs have run and which of them failed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    ran: u64,
    failed: u64,
}

impl Tally {
    /// A boot that has run no proof yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { ran: 0, failed: 0 }
    }

    /// Records that `proof` ran, and whether it passed. A failure stays recorded.
    pub fn record(&mut self, proof: Proof, passed: bool) {
        let bit = 1u64 << (proof as u32);
        self.ran |= bit;
        if !passed {
            self.failed |= bit;
        }
    }

    /// The proofs that have failed, in boot order.
    fn failures(self) -> impl Iterator<Item = Proof> {
        Proof::ALL
            .iter()
            .copied()
            .filter(move |&p| self.failed & (1u64 << (p as u32)) != 0)
    }

    /// The failed proofs whose failure stops the boot, in boot order.
    fn refusals(self) -> impl Iterator<Item = Proof> {
        self.failures()
            .filter(|p| p.consequence() == Consequence::Refuse)
    }

    /// Whether a recorded failure means the kernel must stop.
    #[must_use]
    pub fn must_stop(self) -> bool {
        self.refusals().next().is_some()
    }

    /// Prints why the kernel is stopping: the line a real machine's operator reads last.
    pub fn write_refusal<W: Write>(self, out: &mut W) -> fmt::Result {
        write!(out, "{PREFIX}boot refused: ")?;
        let mut count = 0;
        for proof in self.refusals() {
            if count != 0 {
                out.write_str(", ")?;
            }
            out.write_str(proof.name())?;
            count += 1;
        }
        let what = if count == 1 { "it" } else { "them" };
        writeln!(
            out,
            " failed; the kernel stops rather than run without {what} (ADR-0013)"
        )
    }

    /// Prints what the boot's proofs found, after the last of them. It claims only what the
    /// tally recorded.
    pub fn write_verdict<W: Write>(self, out: &mut W) -> fmt::Result {
        let ran = self.ran.count_ones();
        let failed = self.failed.count_ones();
        let skipped = Proof::ALL.len() as u32 - ran;
        write!(out, "{PREFIX}boot verdict: ")?;
        if failed != 0 || skipped != 0 {
            out.write_str("degraded: ")?;
        }
        write!(out, "{} of {ran} proofs passed", ran - failed)?;
        if skipped != 0 {
            write!(out, "; {skipped} did not run")?;
        }
        if failed != 0 {
            out.write_str("; failed: ")?;
            for (i, proof) in self.failures().enumerate() {
                if i != 0 {
                    out.write_str(", ")?;
                }
                out.write_str(proof.name())?;
                if let Consequence::Withdraw(what) = proof.consequence() {
                    write!(out, " ({what})")?;
                }
            }
        }
        writeln!(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every proof whose failure stops the boot (ADR-0013 R1). Changing this list changes
    /// what a real machine does: change the ADR with it.
    const REFUSING: [Proof; 14] = [
        Proof::PageTables,
        Proof::FrameAllocator,
        Proof::Heap,
        Proof::ApicTimer,
        Proof::NmiStack,
        Proof::CpuState,
        Proof::BootSteps,
        Proof::Preemption,
        Proof::SimdState,
        Proof::UserIsolation,
        Proof::Tlb,
        Proof::WaitQueues,
        Proof::Ipc,
        Proof::Objects,
    ];

    /// Every proof whose failure withholds a capability (ADR-0013 R2).
    const WITHDRAWING: [Proof; 6] = [
        Proof::BusMasterQuiesce,
        Proof::Iommu,
        Proof::Processors,
        Proof::Pcie,
        Proof::BusMastersQuiet,
        Proof::Nvme,
    ];

    fn passed_all() -> Tally {
        let mut tally = Tally::new();
        for &proof in Proof::ALL {
            tally.record(proof, true);
        }
        tally
    }

    fn verdict(tally: Tally) -> String {
        let mut out = String::new();
        tally.write_verdict(&mut out).unwrap();
        out
    }

    fn refusal(tally: Tally) -> String {
        let mut out = String::new();
        tally.write_refusal(&mut out).unwrap();
        out
    }

    #[test]
    fn the_table_has_one_row_per_proof_in_order() {
        assert_eq!(Proof::ALL.len(), 51);
        for (i, &proof) in Proof::ALL.iter().enumerate() {
            assert_eq!(proof as usize, i, "{proof:?} is out of order");
            assert!(!proof.name().is_empty(), "{proof:?} has no name");
        }
        let names: std::collections::BTreeSet<_> = Proof::ALL.iter().map(|p| p.name()).collect();
        assert_eq!(names.len(), Proof::ALL.len(), "two proofs share a name");
    }

    #[test]
    fn the_policy_is_exactly_the_adr() {
        for &proof in Proof::ALL {
            let refuses = proof.consequence() == Consequence::Refuse;
            assert_eq!(refuses, REFUSING.contains(&proof), "{proof:?}");
            let withdraws = matches!(proof.consequence(), Consequence::Withdraw(_));
            assert_eq!(withdraws, WITHDRAWING.contains(&proof), "{proof:?}");
        }
        for p in [
            Proof::BusMasterQuiesce,
            Proof::Iommu,
            Proof::Pcie,
            Proof::BusMastersQuiet,
        ] {
            assert_eq!(p.consequence(), Consequence::Withdraw("no device drivers"));
        }
        assert_eq!(
            Proof::Nvme.consequence(),
            Consequence::Withdraw("no storage")
        );
        assert_eq!(
            Proof::Processors.consequence(),
            Consequence::Withdraw("fewer processors")
        );
    }

    #[test]
    fn only_a_refusing_proof_stops_the_boot() {
        for &proof in Proof::ALL {
            let mut tally = passed_all();
            assert!(!tally.must_stop());
            tally.record(proof, false);
            assert_eq!(tally.must_stop(), REFUSING.contains(&proof), "{proof:?}");
        }
    }

    #[test]
    fn a_passing_or_absent_proof_stops_nothing() {
        let mut tally = Tally::new();
        assert!(!tally.must_stop());
        tally.record(Proof::PageTables, true);
        assert!(!tally.must_stop());
    }

    #[test]
    fn a_failure_cannot_be_recorded_away() {
        let mut tally = Tally::new();
        tally.record(Proof::Tlb, false);
        tally.record(Proof::Tlb, true);
        assert!(tally.must_stop());
        assert_eq!(
            refusal(tally),
            "origin-kernel: boot refused: TLB shootdown failed; the kernel stops rather than run without it (ADR-0013)\n"
        );
    }

    #[test]
    fn a_refusal_names_every_refusing_failure_and_no_other() {
        let mut tally = passed_all();
        tally.record(Proof::Heap, false);
        tally.record(Proof::I8042, false);
        tally.record(Proof::Iommu, false);
        tally.record(Proof::Tlb, false);
        assert_eq!(
            refusal(tally),
            "origin-kernel: boot refused: kernel heap, TLB shootdown failed; the kernel stops rather than run without them (ADR-0013)\n"
        );
    }

    #[test]
    fn the_verdict_of_a_clean_boot_is_exact() {
        assert_eq!(
            verdict(passed_all()),
            "origin-kernel: boot verdict: 51 of 51 proofs passed\n"
        );
    }

    #[test]
    fn the_verdict_says_what_failed_and_what_is_withdrawn() {
        let mut tally = passed_all();
        tally.record(Proof::Iommu, false);
        tally.record(Proof::I8042, false);
        tally.record(Proof::Nvme, false);
        assert_eq!(
            verdict(tally),
            "origin-kernel: boot verdict: degraded: 48 of 51 proofs passed; failed: IOMMU bring-up (no device drivers), NVMe (no storage), i8042\n"
        );
    }

    #[test]
    fn the_verdict_says_how_many_proofs_never_ran() {
        let mut tally = Tally::new();
        tally.record(Proof::Backtrace, true);
        tally.record(Proof::PageTables, true);
        assert_eq!(
            verdict(tally),
            "origin-kernel: boot verdict: degraded: 2 of 2 proofs passed; 49 did not run\n"
        );
    }

    #[test]
    fn the_verdict_counts_a_failure_once() {
        let mut tally = passed_all();
        tally.record(Proof::Raster, false);
        tally.record(Proof::Raster, false);
        assert_eq!(
            verdict(tally),
            "origin-kernel: boot verdict: degraded: 50 of 51 proofs passed; failed: rasterizer\n"
        );
    }

    #[test]
    fn the_verdict_and_the_refusal_are_kernel_lines() {
        let mut tally = passed_all();
        tally.record(Proof::PageTables, false);
        for line in [verdict(tally), refusal(tally)] {
            assert!(line.starts_with(PREFIX), "{line}");
            assert!(line.ends_with('\n'), "{line}");
            assert_eq!(line.matches('\n').count(), 1, "{line}");
        }
    }
}
