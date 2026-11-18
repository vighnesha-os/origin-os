//! `Load` and `LoadTable` (ACPI 6.6 §19.6.77 and §19.6.78; TASKS item 63, step B4, to B4's
//! contract as the coordinating session accepted it on 2026-11-18): a definition block loaded
//! while an evaluation runs.
//!
//! **Every table is checked first** by `origin-acpi-tables`: its header, a length within what
//! holds it, its checksum, and its signature. No byte of its AML runs before (ADR-0019 T1).
//!
//! **The table's term list runs on the engine's frame stack**, inside the running evaluation and
//! against its limits. A [`Loading`] frame below the table's block holds the operator until the
//! block ends ([`Interpreter::finish_load`]); nothing recurses in Rust. Each load is one level of
//! the call-depth limit, so a table that loads itself is bounded as a recursion would be.
//!
//! **A load that fails other than by a limit** (a check, a duplicate name, any error in its term
//! list) is rolled back whole, names and bytes, and its operator gives Zero; the evaluation goes on
//! ([`Interpreter::fail_load_at`], from `run_frames`). A limit ends the evaluation, as everywhere,
//! and the table is rolled back with it. **A load that completes stays**, even if the evaluation
//! around it fails later: AML has no transactions (B4's contract, Q8).
//!
//! **Strict choices**, each recorded in the research ledger (§9):
//! - `Load` takes a block signed `SSDT` only, as §19.6.77 says the block "should" be (Q2);
//! - True is Ones at the integer width (Q4);
//! - `LoadTable` of a `DSDT` or `SSDT` key gives Zero without asking the host: the operating
//!   system loads those itself, as §19.6.78's note says (Q6);
//! - a table loaded twice fails at its first duplicate name (Q7);
//! - a key's ID matches a table's when the table's field holds the key's characters, then NUL or
//!   space to its end (G18);
//! - `_INI` and `_REG` are not run for what a run-time load adds: that is the ACPI service's to
//!   decide (G17, Q9).

use origin_acpi_tables::header::{self as tables, HEADER_LEN, Header};

use crate::Interpreter;
use crate::engine::{BlockKind, Cursor, Frame, Loc, OpFrame, Operand};
use crate::error::{Error, ErrorKind, Limit, Result, err};
use crate::host::{Host, TableKey, Trace};
use crate::mem;
use crate::name::{AmlName, Segs, TextName};
use crate::namespace::{Namespace, Owner};
use crate::object::{NodeId, Object, Val};
use crate::op::Op;
use crate::store::Mode;

/// A table being loaded: below its table block on the frame stack until that block ends. It
/// keeps only what the operator's end needs, so that it is no larger than an operand frame and
/// the frame stack's elements do not grow.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Loading {
    /// `Load` or `LoadTable`.
    op: Op,
    /// The operands its end reads: `Load`'s target; `LoadTable`'s parameter path and data.
    pub(crate) args: [Operand; 2],
    /// The table's index.
    table: u32,
    /// Where the evaluation goes on when the load ends.
    ret: Cursor,
    /// What a failed load unwinds to: activations, creators, holds, and the call depth.
    calls: usize,
    owners: usize,
    held: usize,
    depth: u32,
    /// `LoadTable`'s root path, the base of its parameter path.
    root: NodeId,
}

fn us(x: u32) -> usize {
    usize::try_from(x).unwrap_or(usize::MAX)
}

fn val(f: &OpFrame, i: usize) -> Result<Val> {
    match f.arg(i) {
        Operand::Val(v) => Ok(v),
        _ => err(ErrorKind::Grammar),
    }
}

fn name(f: &OpFrame, i: usize) -> Result<AmlName> {
    match f.arg(i) {
        Operand::Name(n) => Ok(n),
        _ => err(ErrorKind::Grammar),
    }
}

/// The definition block at the start of `bytes`, checked: a header, a declared length within the
/// bytes, a checksum over that length, and the signature `want`. `None` if anything is wrong.
fn checked_span<'a>(bytes: &'a [u8], want: &[u8; 4]) -> Option<tables::Table<'a>> {
    let h = Header::read(bytes).ok()?;
    let span = bytes.get(..usize::try_from(h.length).ok()?)?;
    tables::check_signature(span, want).ok()
}

/// Whether `bytes`, a host's copy of the table under `key`, is that table, sound: a header, a
/// length exactly the copy's, a checksum, and the key's signature and IDs. For `LoadTable` and
/// `DataTableRegion` (the review of B2, B2-14).
pub(crate) fn answers(key: &TableKey, bytes: &[u8]) -> bool {
    tables::check(bytes).is_ok_and(|t| {
        let h = t.header();
        h.signature.0 == key.signature
            && id_matches(&key.oem_id, &h.oem_id)
            && id_matches(&key.oem_table_id, &h.oem_table_id)
    })
}

/// Whether a table's ID field `field` matches a key's `key` (G18, strict until the reading
/// session answers): the key's characters up to its first NUL, then only NUL or space in the
/// field. ACPI tables pad their IDs with spaces; a key from AML strings is padded with NUL.
fn id_matches(key: &[u8], field: &[u8]) -> bool {
    let used = key.iter().position(|&c| c == 0).unwrap_or(key.len());
    let (head, rest) = field.split_at(used.min(field.len()));
    key.get(..used) == Some(head) && rest.iter().all(|&c| c == 0 || c == b' ')
}

impl<H: Host> Interpreter<H> {
    // --- `Load` ------------------------------------------------------------------------------

    /// `Load (Object, Result)` (§19.6.77): the object is a buffer, a SystemMemory region read
    /// whole through the host, or a field unit read as a buffer; the block in it must be an SSDT
    /// wholly inside it. A check that fails gives Zero.
    pub(crate) fn exec_load(&mut self, f: &OpFrame) -> Result<()> {
        let node = self.resolve(name(f, 0)?)?.ok_or(ErrorKind::NotFound)?;
        let bytes = self.load_source(node)?;
        let checked = checked_span(&bytes, b"SSDT");
        #[cfg(test)]
        let checked = checked.or_else(|| {
            crate::tests::planted(crate::tests::Fault::LoadUnchecked)
                .then(|| crate::tests::unchecked_span(&bytes))
                .flatten()
        });
        let started = match checked {
            Some(t) => self.begin_load(f, t, Namespace::root(), Namespace::root()),
            None => {
                mem::release(bytes, &mut self.budget);
                return self.load_result(Op::Load, f.arg(1), false);
            }
        };
        mem::release(bytes, &mut self.budget);
        started
    }

    /// The bytes `Load` reads: a buffer's, a SystemMemory region's whole, a field unit's.
    fn load_source(&mut self, node: NodeId) -> Result<alloc::vec::Vec<u8>> {
        let obj = self.ns.obj(node)?;
        match self.heap.get(obj)? {
            Object::Buffer(b) => mem::copy_bytes(b, &mut self.budget),
            // "Must be in SystemMemory space" (§19.6.77).
            Object::Region(r) if r.space == 0 => {
                let r = *r;
                self.region_bytes(&r)
            }
            Object::Region(_) => err(ErrorKind::Type),
            Object::FieldUnit(_) => {
                let v = self.read_field(node)?;
                self.buffer_of(v)
            }
            _ => err(ErrorKind::Type),
        }
    }

    // --- `LoadTable` -------------------------------------------------------------------------

    /// `LoadTable (Signature, OEMID, OEMTableID, RootPath, ParameterPath, ParameterData)`
    /// (§19.6.78): the table the host finds in the XSDT under the key, loaded under the root path;
    /// after it, the parameter stored. Keys too long are errors; a key signed `DSDT` or `SSDT`,
    /// a table not found, and a copy that fails its checks give Zero.
    pub(crate) fn exec_load_table(&mut self, f: &OpFrame) -> Result<()> {
        let mut key = TableKey {
            signature: [0; 4],
            oem_id: [0; 6],
            oem_table_id: [0; 8],
        };
        for (i, out) in [
            &mut key.signature[..],
            &mut key.oem_id[..],
            &mut key.oem_table_id[..],
        ]
        .into_iter()
        .enumerate()
        {
            let s = self.string_of(val(f, i)?)?;
            let fits = s.len() <= out.len();
            for (d, c) in out.iter_mut().zip(&s) {
                *d = *c;
            }
            mem::release(s, &mut self.budget);
            if !fits {
                return err(ErrorKind::Range);
            }
        }
        // The root path first: a path that names nothing is an error, whether or not the table
        // is there.
        let root = match self.path_operand(f.arg(3))? {
            None => Namespace::root(),
            Some(p) => self.lookup_from(self.scope(), &p)?,
        };
        if key.signature == *b"DSDT" || key.signature == *b"SSDT" {
            return self.load_result(Op::LoadTable, Operand::Empty, false);
        }
        let mut copy = None;
        let present = self
            .host
            .table(&key, &mut |_, bytes| {
                copy = Some(mem::copy_bytes(bytes, &mut self.budget));
            })
            .map_err(ErrorKind::Host)?;
        let bytes = match copy {
            Some(c) if present => c?,
            Some(c) => {
                if let Ok(c) = c {
                    mem::release(c, &mut self.budget);
                }
                return self.load_result(Op::LoadTable, Operand::Empty, false);
            }
            None => return self.load_result(Op::LoadTable, Operand::Empty, false),
        };
        let checked = if answers(&key, &bytes) {
            tables::check(&bytes).ok()
        } else {
            None
        };
        let started = match checked {
            Some(t) => self.begin_load(f, t, root, root),
            None => {
                mem::release(bytes, &mut self.budget);
                return self.load_result(Op::LoadTable, Operand::Empty, false);
            }
        };
        mem::release(bytes, &mut self.budget);
        started
    }

    /// Operand `a` as a path, or `None` for an empty string.
    fn path_operand(&mut self, a: Operand) -> Result<Option<TextName>> {
        let Operand::Val(v) = a else {
            return err(ErrorKind::Grammar);
        };
        let s = self.string_of(v)?;
        let parsed = if s.is_empty() {
            Ok(None)
        } else {
            core::str::from_utf8(&s)
                .map_err(|_| Error::new(ErrorKind::Path))
                .and_then(TextName::parse)
                .map(Some)
        };
        mem::release(s, &mut self.budget);
        parsed
    }

    /// The node `path` names from `scope` by the usual rules, aliases followed: `NotFound` if
    /// none.
    fn lookup_from(&self, scope: NodeId, path: &TextName) -> Result<NodeId> {
        let heap = &self.heap;
        let found = self
            .ns
            .lookup(scope, path, |o| Self::alias_target(heap, o))?
            .ok_or(ErrorKind::NotFound)?;
        self.ns.unalias(found, |o| Self::alias_target(heap, o))
    }

    /// The node `path`'s segments name under `from` exactly, without the search rules: for
    /// `RootPathString.ParameterPathString` (§19.6.78). At most a path's segments, a bound the
    /// code fixes.
    fn lookup_exact(&self, from: NodeId, path: &TextName) -> Result<NodeId> {
        let heap = &self.heap;
        let mut cur = from;
        for i in 0..path.count() {
            cur = self.ns.unalias(cur, |o| Self::alias_target(heap, o))?;
            cur = self
                .ns
                .child(cur, path.seg(i)?)
                .ok_or(ErrorKind::NotFound)?;
        }
        self.ns.unalias(cur, |o| Self::alias_target(heap, o))
    }

    // --- The load itself ---------------------------------------------------------------------

    /// Enters the checked table `t`: a copy of its AML as a table of its own, a [`Loading`] frame
    /// for `f`, a creator for its names, and its block run in `scope`.
    fn begin_load(
        &mut self,
        f: &OpFrame,
        t: tables::Table<'_>,
        scope: NodeId,
        root: NodeId,
    ) -> Result<()> {
        // A load is a level of the call depth: a table that loads itself is bounded.
        let depth = self.run.depth.saturating_add(1);
        if depth > self.config.limits.call_depth {
            return err(ErrorKind::Limit(Limit::Depth));
        }
        let h = *t.header();
        let aml = t.bytes().get(HEADER_LEN..).unwrap_or(&[]);
        let index =
            u32::try_from(self.tables.len()).map_err(|_| ErrorKind::Limit(Limit::Memory))?;
        let end = u32::try_from(aml.len()).map_err(|_| ErrorKind::Truncated)?;
        mem::reserve(&mut self.tables, 1, &mut self.budget)?;
        let aml = mem::copy_bytes(aml, &mut self.budget)?;
        // Cannot fail: the room was reserved before the copy.
        mem::push(
            &mut self.tables,
            crate::Table {
                aml,
                signature: h.signature.0,
                oem_id: h.oem_id,
                oem_table_id: h.oem_table_id,
                failed: false,
            },
            &mut self.budget,
        )?;
        let args = if f.op == Op::Load {
            [f.arg(1), Operand::Empty]
        } else {
            [f.arg(4), f.arg(5)]
        };
        let loading = Loading {
            op: f.op,
            args,
            table: index,
            ret: self.cur,
            calls: self.calls.len(),
            owners: self.owners.len(),
            held: self.held.len(),
            depth: self.run.depth,
            root,
        };
        // Only the memory limit can stop what follows, and then the evaluation ends: once the
        // `Loading` frame is pushed, `abort` fails the table with it and tells a tracing host so;
        // before, it is failed here, and the host was told nothing of it. (Today that push always
        // has room: the operator's own frame was popped just before it.)
        if let Err(e) = self.push_frame(Frame::Loading(loading)) {
            self.fail_table(index);
            return Err(e);
        }
        self.trace(&Trace::Load {
            signature: h.signature.0,
            oem_table_id: h.oem_table_id,
        });
        self.push_owner(Owner::Table(index))?;
        self.run.depth = depth;
        self.cur = Cursor {
            table: index,
            pc: 0,
        };
        self.push_frame(Frame::Block {
            kind: BlockKind::Table,
            end,
            scope,
            pred: 0,
        })
    }

    /// The table's block has ended: its names stay, the operator gets True, and `LoadTable`'s
    /// parameter is stored. Load-time code that ends holding a mutex fails the load instead, as a
    /// table's load at the top level does (§19.6.89; G2).
    pub(crate) fn finish_load(&mut self, l: Loading) -> Result<()> {
        self.pop_frame();
        if self.release_held(l.held) {
            let at = self.frames.len();
            return self.fail_load(at, l, Error::new(ErrorKind::MutexHeld));
        }
        self.pop_owner(false);
        self.run.depth = l.depth;
        self.cur = l.ret;
        self.trace(&Trace::Loaded(Ok(l.table)));
        if l.op == Op::LoadTable {
            self.store_parameter(&l)?;
        }
        let [target, _] = l.args;
        self.load_result(l.op, target, true)
    }

    /// `LoadTable`'s parameter, after the table is in the namespace: `ParameterData` stored into
    /// the object `ParameterPath` names, as written if it begins with `\` or `^`, else under the
    /// root path. An object that does not exist is an error; the table stays loaded.
    fn store_parameter(&mut self, l: &Loading) -> Result<()> {
        let [path, data] = l.args;
        let Some(path) = self.path_operand(path)? else {
            return Ok(());
        };
        let node = if path.root() || path.parents() > 0 {
            self.lookup_from(self.scope(), &path)?
        } else {
            self.lookup_exact(l.root, &path)?
        };
        let Operand::Val(data) = data else {
            return err(ErrorKind::Grammar);
        };
        self.store(Loc::Node(node), data, Mode::Store).map(|_| ())
    }

    /// Whether a `Return` would leave a table being loaded: the nearest of a method's call and a
    /// [`Loading`] frame is the load's. Its table-level code has no method to return from, and
    /// returning from the method around the `Load` would skip the load's end; it is `Grammar`, as
    /// a `Return` outside any method is.
    pub(crate) fn return_crosses_load(&self) -> bool {
        if self.loading == 0 {
            return false;
        }
        let nearest = self
            .frames
            .iter()
            .rev()
            .position(|f| matches!(f, Frame::Call | Frame::Loading(_)));
        self.note_work(u64::try_from(nearest.unwrap_or(0)).unwrap_or(u64::MAX));
        nearest
            .and_then(|k| self.frames.iter().rev().nth(k))
            .is_some_and(|f| matches!(f, Frame::Loading(_)))
    }

    /// The nearest load an error `e` inside the evaluation above `base` fails: the top
    /// [`Loading`] frame, unless `e` is a limit, which ends the evaluation whatever it was in.
    pub(crate) fn failing_load(&self, base: usize, e: &Error) -> Option<usize> {
        if self.loading == 0 || matches!(e.kind, ErrorKind::Limit(_) | ErrorKind::Pending) {
            return None;
        }
        let at = self
            .frames
            .iter()
            .enumerate()
            .skip(base)
            .rev()
            .find(|(_, f)| matches!(f, Frame::Loading(_)))
            .map(|(k, _)| k);
        // The frames scanned are work, counted with the next charge.
        let scanned = self.frames.len().saturating_sub(at.unwrap_or(base));
        self.note_work(u64::try_from(scanned).unwrap_or(u64::MAX));
        at
    }

    /// Fails the load whose [`Loading`] frame is at `at`, after `e`.
    pub(crate) fn fail_load_at(&mut self, at: usize, e: Error) -> Result<()> {
        let Some(Frame::Loading(l)) = self.frames.get(at).copied() else {
            return Err(e);
        };
        self.fail_load(at, l, e)
    }

    /// Rolls the load `l` back: the frames from `at`, the activations, creators (and so the
    /// table's names) and holds above its marks, and its bytes; then its operator gets Zero.
    fn fail_load(&mut self, at: usize, l: Loading, e: Error) -> Result<()> {
        // Planted, the table's creator forgets its names before the unwind, which then leaves
        // them in the namespace.
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::KeepFailedLoad)
            && let Some(ctx) = self.owners.get_mut(l.owners)
        {
            let kept = core::mem::take(&mut ctx.created);
            mem::release(kept, &mut self.budget);
        }
        self.unwind(at, l.calls, l.owners);
        self.release_held(l.held);
        self.pending = None;
        self.run.depth = l.depth;
        self.fail_table(l.table);
        self.cur = l.ret;
        self.trace_limit(&e);
        self.trace(&Trace::Loaded(Err(e)));
        let [target, _] = l.args;
        self.load_result(l.op, target, false)
    }

    /// Marks table `index` failed and gives its bytes back, as a failed top-level load does.
    fn fail_table(&mut self, index: u32) {
        if let Some(t) = self.tables.get_mut(us(index)) {
            t.failed = true;
            let aml = core::mem::take(&mut t.aml);
            mem::release(aml, &mut self.budget);
        }
    }

    /// Fails every table a [`Loading`] frame above `frames` was loading, when an evaluation is
    /// aborted (`abort`), and says how many: their names go with their creators there, and
    /// `abort` tells a tracing host each failed. One pass over the frames, as the unwind's own.
    pub(crate) fn fail_tables_loading(&mut self, frames: usize) -> usize {
        if self.loading == 0 {
            return 0;
        }
        let mut failed = 0usize;
        let mut k = frames;
        while let Some(f) = self.frames.get(k).copied() {
            if let Frame::Loading(l) = f {
                self.fail_table(l.table);
                failed = failed.saturating_add(1);
            }
            k = k.saturating_add(1);
        }
        failed
    }

    /// The operator's value: True (Ones) if the table loaded, else Zero; `Load` also stores it in
    /// its target (§19.6.77).
    fn load_result(&mut self, op: Op, target: Operand, loaded: bool) -> Result<()> {
        #[cfg(test)]
        let loaded = loaded != crate::tests::planted(crate::tests::Fault::InvertLoadResult);
        let v = Val::Int(if loaded { self.width.mask() } else { 0 });
        if op == Op::Load {
            let Operand::Loc(l) = target else {
                return err(ErrorKind::Grammar);
            };
            self.store(l, v, Mode::Store)?;
        }
        self.give_val(v)
    }
}
