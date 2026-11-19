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
//!
//! **The load policy** (G1 and G3; the ledger's §2). Load-time code is a table's table level and
//! the bodies of the `Scope`, `Device`, `Processor`, `PowerResource`, `ThermalZone`, `If`, `Else`
//! and `While` in it, not the methods it calls. A statement there that fails is abandoned whole:
//!
//! - what it started above its block is unwound, a method it called ending as at any error, its
//!   temporary names removed (what `Acquire` took stays held: G2);
//! - it is skipped to its structural end by parsing it again from its start, running nothing,
//!   creating nothing and asking the host nothing: a name in a statement or `TermArg` position is
//!   looked up only to learn whether it is a method and how many arguments it takes, one that
//!   resolves to nothing taking none (G1), and a name in a place (a `SuperName`, a `Target`, a
//!   `SimpleName`) is stepped over as a name, never a call, as the decoder reads it (the review of
//!   B5, B5-R1). A term is a unit of work. The work list is charged to the memory limit and given
//!   back;
//! - a statement with a package length is skipped by it, so a failing scope-opener loses its
//!   whole body; an `If` abandoned in its predicate takes the `Else` after it, and a `While`'s
//!   predicate, failing on any pass, abandons the whole `While`;
//! - the load goes on at the next statement, and [`Report::Abandoned`] tells the host.
//!
//! What the parser cannot step past fails the table whole and rolls it back, as before step B5:
//! [`whole_table`]'s kinds. **Skipping the whole statement is Origin's choice**: the ledger says
//! parsing goes on from where the failure stopped, without reading the unwinding in detail (its
//! §5); for a lone unresolved name, the common forward call, the two agree (the coordinating
//! session's ruling on B5's contract, Q1).
//!
//! **Names in packages** (G1): a name that load-time code puts in a package is bound once, by
//! [`Interpreter::bind_deferred`] after every boot table has loaded, or at the end of the run-time
//! load that made it, whether the table loaded or failed; one that still names nothing becomes an
//! empty element, reported. Only names something can still read then are bound or reported:
//! the pass first marks what is reachable, as the collector does, and the list of names waiting
//! keeps nothing alive (the review of B5's answers, B5-R10 and B5-R11). A package made inside a
//! method resolves its names when they are read.
//!
//! **A table already loaded** (B4's Q7, as ruled for step B5): a block with the signature, IDs and
//! term list of a table loaded and not failed is refused whole, before anything of it is created.

use alloc::vec::Vec;

use crate::Interpreter;
use crate::engine::{BlockKind, CALL_KINDS, ELSE, Frame};
use crate::error::{At, Error, ErrorKind, Result, err};
use crate::host::{Host, Report};
use crate::mem;
use crate::name;
use crate::object::{Bind, NameElem, NodeId, ObjRef, Object};
use crate::op::{self, Kind};

/// The most reports one top-level evaluation gives its host (the coordinating session's ruling
/// on B5's contract, Q11): enough for every tolerance in the tables seen so far, few enough that
/// firmware cannot flood the host.
pub const REPORTS: u64 = 64;

/// The opcode of `If`.
const IF: u8 = 0xA0;

/// Whether an error of `kind` fails a table whole: what the parser cannot step past, a limit, and
/// `Fatal` (the coordinating session's ruling on B5's contract, Q2). `Pending` is the engine's
/// own and never abandons anything. Every other kind fails only its statement.
pub(crate) const fn whole_table(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::Truncated
            | ErrorKind::PackageLength
            | ErrorKind::Reserved { .. }
            | ErrorKind::Name
            | ErrorKind::Grammar
            | ErrorKind::Limit(_)
            | ErrorKind::Fatal { .. }
            | ErrorKind::Pending
    )
}

fn us(x: u32) -> usize {
    usize::try_from(x).unwrap_or(usize::MAX)
}

/// One level of the skip's work list: an operator's operand kinds, and the next to step over.
type Level = (&'static [Kind], usize);

impl<H: Host> Interpreter<H> {
    /// Tells the host of a tolerance, or of a store in pieces, within the bound.
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

    /// Tells the host that `node`'s mutex, taken by `method`, was released at an end other than
    /// its `Release`: their paths, written without allocating.
    pub(crate) fn tell_released(&mut self, node: Option<NodeId>, method: Option<NodeId>) {
        let mut buf = [[0u8; 4]; 32];
        let mut by = [[0u8; 4]; 32];
        let path = node
            .and_then(|n| self.ns.path_into(n, &mut buf))
            .unwrap_or(&[]);
        let method = method
            .and_then(|n| self.ns.path_into(n, &mut by))
            .unwrap_or(&[]);
        self.tell(&Report::Released {
            mutex: path,
            method,
        });
    }

    /// Tells the host that a store to field unit `node` goes in `pieces` pieces: its path,
    /// written without allocating.
    pub(crate) fn tell_pieces(&mut self, node: NodeId, pieces: u64) {
        // The planted fault `SilentPieces`: the store goes in pieces and the host is not told.
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::SilentPieces) {
            return;
        }
        let mut buf = [[0u8; 4]; 32];
        let path = self.ns.path_into(node, &mut buf).unwrap_or(&[]);
        self.tell(&Report::Pieces {
            field: path,
            pieces,
        });
    }

    // --- The load policy -----------------------------------------------------------------------

    /// Whether the code running is load-time code: a boot load's (no activation at all), or the
    /// table level of the innermost table `Load` or `LoadTable` is loading (B4-13's mark).
    pub(crate) fn in_load_time_code(&self) -> bool {
        if self.loading > 0 {
            self.calls.len() == self.load_calls
        } else {
            self.calls.is_empty()
        }
    }

    /// The frame index of the block of load-time code whose statement `e` fails, if `e` is a
    /// statement's failure there: the topmost block above the innermost table being loaded (or
    /// above `base`) and below every method's block above it. `None` if `e` fails the table whole,
    /// or if no load-time code is running below where it happened.
    pub(crate) fn abandon_target(&self, base: usize, e: &Error) -> Option<usize> {
        if whole_table(e.kind) {
            return None;
        }
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::NoAbandon) {
            return None;
        }
        let frames = self.frames.get(base..)?;
        self.note_work(u64::try_from(frames.len()).unwrap_or(u64::MAX));
        let lo = frames
            .iter()
            .rposition(|f| matches!(f, Frame::Loading(_)))
            .map_or(0, |i| i.saturating_add(1));
        let above = frames.get(lo..)?;
        let methods = above
            .iter()
            .position(|f| {
                matches!(
                    f,
                    Frame::Block {
                        kind: BlockKind::Method,
                        ..
                    }
                )
            })
            .unwrap_or(above.len());
        let b = above
            .get(..methods)?
            .iter()
            .rposition(|f| matches!(f, Frame::Block { .. }))?;
        Some(base.saturating_add(lo).saturating_add(b))
    }

    /// Abandons the statement of load-time code that block `b` is running (see the module's
    /// documentation): unwinds what is above the block, skips the statement, reports it, and lets
    /// the load go on.
    ///
    /// # Errors
    /// A failure the skip meets (malformed AML, a limit): the table fails whole.
    pub(crate) fn abandon(&mut self, b: usize, e: Error) -> Result<()> {
        let Some(Frame::Block { stmt, end, .. }) = self.frames.get(b).copied() else {
            return err(ErrorKind::Grammar);
        };
        let above = b.saturating_add(1);
        let calls = self
            .frames
            .get(above..)
            .map_or(0, |f| f.iter().filter(|f| matches!(f, Frame::Call)).count());
        let keep = self.calls.len().saturating_sub(calls);
        // A serialized method among them gives back its own mutex, as its return would; what
        // `Acquire` took stays held until the evaluation ends (G2).
        let mut i = keep;
        while let Some(c) = self.calls.get(i).copied() {
            if c.serialized {
                self.drop_implicit(c.serial);
            }
            i = i.saturating_add(1);
        }
        let owners = self.owners.len().saturating_sub(calls);
        self.unwind(above, keep, owners);
        self.run.depth = self
            .run
            .depth
            .saturating_sub(u32::try_from(calls).unwrap_or(u32::MAX));
        self.cur = stmt;
        let resumed = self.skip_statement(stmt.pc, end)?;
        self.cur.pc = resumed;
        // What the statement put in packages is bound by the pass if something can still read
        // it, an earlier pass's store of a `While` or an inner `Store`'s included (B5-R10).
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::MarkAbandoned) {
            self.planted_never_bind(stmt.table, stmt.pc, resumed);
        }
        if let Some(t) = self.tables.get_mut(us(stmt.table)) {
            t.abandoned = t.abandoned.saturating_add(1);
        }
        self.tell(&Report::Abandoned {
            error: e,
            statement: At {
                table: stmt.table,
                offset: stmt.pc,
            },
            resumed: At {
                table: stmt.table,
                offset: resumed,
            },
        });
        Ok(())
    }

    /// The end of the statement at `start`, no further than `end`, found without running it; an
    /// `If` takes the `Else` right after it.
    fn skip_statement(&mut self, start: u32, end: u32) -> Result<u32> {
        let mut work: Vec<Level> = Vec::new();
        let r = self.skip_terms(&mut work, start, end);
        mem::release(work, &mut self.budget);
        let pc = r?;
        if self.byte(start)? == IF && pc < end && self.byte(pc)? == ELSE {
            let at = add(pc, 1)?;
            let (len, _) = self.pkg_length(at)?;
            let after = add(at, len)?;
            if after > end {
                return err(ErrorKind::PackageLength);
            }
            return Ok(after);
        }
        Ok(pc)
    }

    /// Steps over one term at `start` and everything it takes, without recursion: a work list of
    /// the operand kinds still to step over.
    fn skip_terms(&mut self, work: &mut Vec<Level>, start: u32, end: u32) -> Result<u32> {
        const ONE: &[Kind] = &[Kind::TermArg];
        let mut pc = start;
        mem::push(work, (ONE, 0), &mut self.budget)?;
        while let Some(top) = work.last_mut() {
            let (kinds, next) = *top;
            let Some(&kind) = kinds.get(next) else {
                work.pop();
                continue;
            };
            top.1 = next.saturating_add(1);
            self.charge(1)?;
            pc = match kind {
                Kind::Byte => add(pc, 1)?,
                Kind::Word => add(pc, 2)?,
                Kind::DWord => add(pc, 4)?,
                Kind::NameNew | Kind::NameOld | Kind::NameRaw => name::parse(self.aml(), pc)?.1,
                _ => self.skip_term(work, kind, pc, end)?,
            };
            if pc > end {
                return err(ErrorKind::PackageLength);
            }
        }
        Ok(pc)
    }

    /// Steps over the start of the term at `pc`, an operand of `kind`: a data object, a local, an
    /// argument or a name whole, an operator with a package length whole; an operator's or a
    /// call's operands are pushed onto `work`. Returns where the term's own bytes end.
    fn skip_term(&mut self, work: &mut Vec<Level>, kind: Kind, pc: u32, end: u32) -> Result<u32> {
        let b = self.byte(pc)?;
        let next = add(pc, 1)?;
        match b {
            0x00 | 0x01 | 0xFF | 0x60..=0x6E => return Ok(next),
            0x0A => return add(pc, 2),
            0x0B => return add(pc, 3),
            0x0C => return add(pc, 5),
            0x0E => return add(pc, 9),
            0x0D => return self.skip_string(next, end),
            _ => {}
        }
        // `Revision` and `Debug`.
        if b == 0x5B && matches!(self.byte(next).ok(), Some(0x30 | 0x31)) {
            return add(pc, 2);
        }
        if name::starts_name(b) {
            let (n, after) = name::parse(self.aml(), pc)?;
            // A name in a place is never called (`decode_location`; ledger 64 §3 G1: a call only
            // where one is possible): only a statement or a `TermArg` may be one (B5-R1).
            #[cfg(test)]
            let anywhere = crate::tests::planted(crate::tests::Fault::SkipCallsInPlaces);
            #[cfg(not(test))]
            let anywhere = false;
            if !anywhere && !matches!(kind, Kind::TermArg | Kind::TermArgRaw) {
                return Ok(after);
            }
            let argc = match self.resolve(n) {
                Ok(Some(node)) => match self.heap.get(self.ns.obj(node)?)? {
                    Object::Method(m) => m.argc(),
                    Object::Osi => 1,
                    _ => 0,
                },
                // A name that resolves to nothing is a plain name taking no arguments (G1).
                _ => 0,
            };
            if let Some(kinds) = CALL_KINDS.get(..usize::from(argc))
                && !kinds.is_empty()
            {
                mem::push(work, (kinds, 0), &mut self.budget)?;
            }
            return Ok(after);
        }
        if b == ELSE {
            return err(ErrorKind::Grammar);
        }
        let info = op::decode(b, self.byte(next).ok()).ok_or(ErrorKind::Reserved {
            first: b,
            second: (b == 0x5B).then(|| self.byte(next).ok()).flatten(),
        })?;
        let at = add(pc, info.len)?;
        if info.pkg {
            let (len, _) = self.pkg_length(at)?;
            return add(at, len);
        }
        if !info.args.is_empty() {
            mem::push(work, (info.args, 0), &mut self.budget)?;
        }
        Ok(at)
    }

    /// Steps over a string literal's characters from `from`: past its NUL, or to `end` if it has
    /// none before it (G13, as the decoder reads one; nothing is reported for a literal skipped).
    fn skip_string(&mut self, from: u32, end: u32) -> Result<u32> {
        let rest = self
            .aml()
            .get(us(from)..us(end).max(us(from)))
            .ok_or(ErrorKind::Truncated)?;
        let Some(len) = rest.iter().position(|&c| c == 0) else {
            return Ok(end);
        };
        self.note_work(
            u64::try_from(len / 64)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        );
        let len = u32::try_from(len).map_err(|_| ErrorKind::Truncated)?;
        add(add(from, len)?, 1)
    }

    /// The loaded, not failed table with this identity and term list, if any (B4's Q7, as ruled
    /// for step B5). A unit of work for each 64 bytes compared.
    pub(crate) fn identical_table(
        &self,
        signature: [u8; 4],
        oem_id: [u8; 6],
        oem_table_id: [u8; 8],
        aml: &[u8],
    ) -> Option<u32> {
        let mut found = None;
        for (i, t) in self.tables.iter().enumerate() {
            self.note_work(1);
            if t.failed
                || t.signature != signature
                || t.oem_id != oem_id
                || t.oem_table_id != oem_table_id
                || t.aml.len() != aml.len()
            {
                continue;
            }
            self.note_work(u64::try_from(aml.len() / 64).unwrap_or(u64::MAX));
            if t.aml.as_slice() == aml {
                found = u32::try_from(i).ok();
                break;
            }
        }
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::LoadTwice) {
            return None;
        }
        found
    }

    // --- Names in packages ---------------------------------------------------------------------

    /// The name element `r` is, if it is one.
    fn elem(&self, r: ObjRef) -> Option<NameElem> {
        match self.heap.get(r) {
            Ok(Object::NameElem(e)) => Some(*e),
            _ => None,
        }
    }

    fn set_bind(&mut self, r: ObjRef, bind: Bind) {
        if let Ok(Object::NameElem(e)) = self.heap.get_mut(r) {
            e.bind = bind;
        }
    }

    /// Binds the names load-time code put in packages that are not bound yet and that something
    /// can still read: those of table `table`, a run-time load's, which are the list's last (a
    /// load nested in it has bound its own at its end), or every one. Each is bound to its node,
    /// or left empty and reported. Returns how many were left empty.
    ///
    /// What can be read is what a marking from the collector's roots reaches (the review of B5's
    /// answers, B5-R10 and B5-R11): the list is pruned to it first, and nothing is freed, so the
    /// pass may run where the collector may not (a run for a PCI_Config region's function outside
    /// the frames, `pci.rs`). A name only a value held outside the frames there could still read
    /// is taken off unbound, and is looked up when read, as a method's is. A name the marking did
    /// not reach was unreachable at every earlier collection too, which took off only what was
    /// unreachable then: so the pass binds and reports the same whether or not the collector ran.
    ///
    /// The marking's work, as a collection's, and a unit an entry, the run-time load's only (the
    /// review of B5, B5-R2), are charged before anything is bound. No marking when no entry waits.
    /// It allocates nothing.
    pub(crate) fn bind_names(&mut self, table: Option<u32>) -> Result<usize> {
        let ours =
            |i: &Self, r: ObjRef| table.is_none_or(|t| i.elem(r).is_some_and(|e| e.table == t));
        if !self.deferred.last().is_some_and(|&r| ours(self, r)) {
            return Ok(0);
        }
        let marked = self.sift_deferred();
        let mut from = self.deferred.len();
        while let Some(&r) = from.checked_sub(1).and_then(|k| self.deferred.get(k)) {
            if !ours(self, r) {
                break;
            }
            from = from.saturating_sub(1);
        }
        let entries = self.deferred.len().saturating_sub(from);
        self.charge(marked.saturating_add(u64::try_from(entries).unwrap_or(u64::MAX)))?;
        #[cfg(test)]
        {
            self.run.pass_entries = self
                .run
                .pass_entries
                .saturating_add(u64::try_from(entries).unwrap_or(u64::MAX));
        }
        let mut empty = 0usize;
        let mut k = from;
        while let Some(&r) = self.deferred.get(k) {
            k = k.saturating_add(1);
            #[cfg(test)]
            if crate::tests::planted(crate::tests::Fault::NoBind) {
                break;
            }
            let Some(e) = self.elem(r) else {
                continue;
            };
            if e.bind != Bind::Deferred {
                continue;
            }
            let bound = match self.resolve_elem(e) {
                Ok(n) => Bind::Node(n),
                Err(_) => {
                    empty = empty.saturating_add(1);
                    self.tell(&Report::UnboundElement {
                        at: At {
                            table: e.table,
                            offset: e.at,
                        },
                    });
                    Bind::Empty
                }
            };
            self.set_bind(r, bound);
        }
        self.deferred.truncate(from);
        Ok(empty)
    }

    /// Takes off the deferred list what nothing can read: a marking from the collector's roots,
    /// freeing nothing. Returns its work, counted as a collection's marking is. Planted
    /// (`NoSift`), the pass takes the list as it is, so what it binds depends on the collector.
    fn sift_deferred(&mut self) -> u64 {
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::NoSift) {
            return 0;
        }
        let work = self.root_work();
        if self.mark_reachable() {
            self.prune_deferred();
        }
        self.heap.clear_marks();
        work.saturating_add(self.heap.take_traced())
    }

    /// The marking B5-R3 made and the review of its answers found wrong (B5-R10), planted: the
    /// names an abandoned statement made, found by its byte range among the list's last, taken off
    /// and never bound, though an earlier pass of a `While` may have stored them where they are
    /// read.
    #[cfg(test)]
    fn planted_never_bind(&mut self, table: u32, from: u32, to: u32) {
        let mut k = self.deferred.len();
        while let Some(&r) = k.checked_sub(1).and_then(|j| self.deferred.get(j)) {
            if !self
                .elem(r)
                .is_some_and(|e| e.table == table && (from..to).contains(&e.at))
            {
                break;
            }
            self.set_bind(r, Bind::Lazy);
            k = k.saturating_sub(1);
        }
        self.deferred.truncate(k);
    }

    /// Binds the names load-time code put in packages, once every boot table has loaded (G1, as
    /// step B5 builds it): each to the node it names, or, if it names nothing, empty, reading as
    /// an uninitialized element, and reported ([`Report::UnboundElement`]). Only names not yet
    /// bound are; [`Interpreter::initialize`] runs this first, and a run-time load binds its own
    /// table's at its end. Returns how many were left empty. An evaluation of its own: its reports
    /// are bounded as any evaluation's.
    ///
    /// # Errors
    /// [`ErrorKind::Limit`] if the pass reached the limits.
    pub fn bind_deferred(&mut self) -> Result<usize> {
        let marks = self.begin_evaluation()?;
        let r = self.bind_names(None);
        if let Err(e) = &r {
            self.abort(marks, e);
        }
        self.end_evaluation();
        r
    }
}

fn add(a: u32, b: u32) -> Result<u32> {
    a.checked_add(b)
        .ok_or_else(|| Error::new(ErrorKind::Truncated))
}
