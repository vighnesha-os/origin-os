//! What the interpreter tolerates, and how it tells its host (TASKS item 63, step B5; the
//! coordinating session's rulings of 2026-11-19 on TASKS 64's ledger,
//! `docs/research/2026-11-14-aml-gaps-acpica.md`).
//!
//! **The principle.** Follow ACPICA's default wherever the ledger says deployed firmware or other
//! implementations rely on it, never its lenient mode, and tell the host of every tolerance: a
//! [`Report`], never silence. Nothing here raises a limit or lets AML reach anything new.
//!
//! **Reports.** Each top-level evaluation gives its host at most [`REPORTS`] reports; the rest are
//! counted and told in one [`Report::Suppressed`] as it ends, so that firmware cannot flood the
//! host. A report is a unit of work, given or not.

use crate::Interpreter;
use crate::host::{Host, Report};
use crate::object::NodeId;

/// The most reports one top-level evaluation gives its host (the coordinating session's ruling
/// on B5's contract, Q11): enough for every tolerance in the tables seen so far, few enough that
/// firmware cannot flood the host.
pub const REPORTS: u64 = 64;

impl<H: Host> Interpreter<H> {
    /// Tells the host of a tolerance, within the bound.
    pub(crate) fn tell(&mut self, report: &Report<'_>) {
        self.note_work(1);
        self.run.reports = self.run.reports.saturating_add(1);
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::NoReport) {
            return;
        }
        if self.run.reports <= REPORTS {
            self.host.report(report);
        }
    }

    /// As an evaluation ends: the reports past the bound, as one count.
    pub(crate) fn tell_suppressed(&mut self) {
        let over = self.run.reports.saturating_sub(REPORTS);
        if over > 0 {
            self.host.report(&Report::Suppressed { count: over });
        }
    }

    /// Tells the host that `node`'s mutex was still held when the outermost evaluation ended:
    /// its path, written without allocating.
    pub(crate) fn tell_released(&mut self, node: Option<NodeId>) {
        let mut buf = [[0u8; 4]; 32];
        let path = node
            .and_then(|n| self.ns.path_into(n, &mut buf))
            .unwrap_or(&[]);
        self.tell(&Report::Released { mutex: path });
    }
}
