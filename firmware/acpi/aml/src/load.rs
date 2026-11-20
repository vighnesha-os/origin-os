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
//! **Every store a load makes is ready before anything changes** (the review of B4, B4-1).
//! `Load`'s target, and `LoadTable`'s parameter, may be a field of a PCI_Config region whose
//! function is not yet found, and a store there asks for it (`Pending`) and is put back. So, as
//! `Divide` does, each is made ready first. `Load`'s target is made ready before its source is
//! read. `LoadTable`'s parameter is stored by a step of its own, a [`Parameter`] frame pushed
//! once the load has ended (the review's addendum, B4-12): it finds the node, makes it ready and
//! stores, so that a `Pending` puts that step back alone, and any failure there, a limit
//! included, comes after the table was added and leaves it loaded (§19.6.78). A `Pending` that
//! still reaches a load's store is the interpreter's own error (`Grammar`), never a retry.
//!
//! **A load is best-effort** (step B5; `tolerate.rs`): a statement of its load-time code that
//! fails as it runs, a duplicate name or an unresolved one among them, is abandoned and reported,
//! and the table loads. **A load that fails whole** (a check, a block already loaded, a failure
//! the parser cannot step past) is rolled back whole, names and bytes, and its operator gives
//! Zero; the evaluation goes on ([`Interpreter::fail_load_at`], from `run_frames`). A limit or a `Fatal` ends the evaluation,
//! as everywhere, and the table is rolled back with it (`Fatal`: B4-2). **A load that completes
//! stays**, even if the evaluation around it fails later: AML has no transactions (B4's contract,
//! Q8).
//!
//! **Strict choices**, each recorded in the research ledger (§9):
//! - `Load` takes a block signed `SSDT` only, as §19.6.77 says the block "should" be (Q2);
//! - True is Ones at the integer width (Q4);
//! - `LoadTable` of a `DSDT` or `SSDT` key gives Zero without asking the host: the operating
//!   system loads those itself, as §19.6.78's note says (Q6);
//! - a table already loaded, the same signature, IDs and term list as one loaded and not
//!   failed, is refused whole and gives Zero (Q7, as ruled for step B5; `tolerate.rs`);
//! - a key's signature is four AML name characters; an empty OEM ID or OEM table ID matches any
//!   table, any other is compared byte for byte, padded with NUL (G18, TASKS 64's ledger §3;
//!   step B5);
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
    /// The activations mark of the table being loaded around this one, or `usize::MAX`: what
    /// [`Interpreter::at_loaded_table_level`] goes back to when this load ends (B4-13).
    outer_calls: usize,
}

impl Loading {
    /// The activations there were when this load began: its table level's.
    pub(crate) const fn calls_mark(&self) -> usize {
        self.calls
    }

    /// The same mark for the load around this one, or `usize::MAX`.
    pub(crate) const fn outer_calls(&self) -> usize {
        self.outer_calls
    }
}

/// `LoadTable`'s parameter, stored by a step of its own once the load has ended (the review of
/// B4's addendum, B4-12): `ParameterPath` and `ParameterData`, and the root path the first is taken
/// under.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Parameter {
    /// `ParameterPath` and `ParameterData`.
    pub(crate) args: [Operand; 2],
    /// `LoadTable`'s root path.
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

/// Whether a table's ID field `field` matches a key's `key` (G18, TASKS 64's ledger §3; step
/// B5): an empty key, all NUL, matches any table; any other is compared byte for byte, padded
/// with NUL, so a table's trailing spaces must be in the key.
fn id_matches(key: &[u8], field: &[u8]) -> bool {
    #[cfg(test)]
    if crate::tests::planted(crate::tests::Fault::OldIdMatch) {
        let used = key.iter().position(|&c| c == 0).unwrap_or(key.len());
        let (head, rest) = field.split_at(used.min(field.len()));
        return key.get(..used) == Some(head) && rest.iter().all(|&c| c == 0 || c == b' ');
    }
    key.iter().all(|&c| c == 0) || key == field
}

/// Whether `c` may be in a table signature `LoadTable` or `DataTableRegion` is given: an AML
/// name character (G18; B5's contract, Q8).
const fn signature_char(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'
}

impl<H: Host> Interpreter<H> {
    /// The key `LoadTable` and `DataTableRegion` look a table up by, from their three strings
    /// (G18, TASKS 64's ledger §3; step B5): the signature's first four characters, which must be
    /// AML name characters (`Name` otherwise, a shorter string included), the rest of it not
    /// compared; the OEM ID and OEM table ID padded with NUL, each `Range` if longer than its
    /// field.
    pub(crate) fn table_key(&mut self, strings: [Val; 3]) -> Result<TableKey> {
        let mut key = TableKey {
            signature: [0; 4],
            oem_id: [0; 6],
            oem_table_id: [0; 8],
        };
        for (i, (out, v)) in [
            &mut key.signature[..],
            &mut key.oem_id[..],
            &mut key.oem_table_id[..],
        ]
        .into_iter()
        .zip(strings)
        .enumerate()
        {
            let s = self.string_of(v)?;
            let fits = if i == 0 {
                s.get(..4)
                    .is_some_and(|sig| sig.iter().all(|&c| signature_char(c)))
            } else {
                s.len() <= out.len()
            };
            for (d, c) in out.iter_mut().zip(&s) {
                *d = *c;
            }
            mem::release(s, &mut self.budget);
            if !fits {
                return err(if i == 0 {
                    ErrorKind::Name
                } else {
                    ErrorKind::Range
                });
            }
        }
        Ok(key)
    }

    // --- `Load` ------------------------------------------------------------------------------

    /// `Load (Object, Result)` (§19.6.77): the object is a buffer, a SystemMemory region read
    /// whole through the host, or a field unit read as a buffer; the block in it must be an SSDT
    /// wholly inside it. A check that fails gives Zero.
    pub(crate) fn exec_load(&mut self, f: &OpFrame) -> Result<()> {
        // The target first: a step put back for its PCI function has read and changed nothing,
        // and every later store into it, the failed check's Zero included, is ready (B4-1).
        if let Operand::Loc(target) = f.arg(1) {
            self.load_ready(target)?;
        }
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
        let key = self.table_key([val(f, 0)?, val(f, 1)?, val(f, 2)?])?;
        // The root path first: a path that names nothing is an error, whether or not the table
        // is there. It must name a scope object, as `Scope`'s does: the root, a predefined scope,
        // a device, processor, thermal zone or power resource (§19.6.122; the review of B4, B4-4,
        // a strict choice the ledger records).
        let root = match self.path_operand(f.arg(3))? {
            None => Namespace::root(),
            Some(p) => {
                let node = self.lookup_from(self.scope(), &p)?;
                if !matches!(self.heap.get(self.ns.obj(node)?)?, Object::Scope { .. }) {
                    return err(ErrorKind::WrongObject);
                }
                node
            }
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
        // A table already loaded is refused whole, and the operator gives Zero (B4's Q7, as ruled
        // for step B5).
        if let Some(table) = self.identical_table(h.signature.0, h.oem_id, h.oem_table_id, aml) {
            self.tell(&crate::Report::AlreadyLoaded { table });
            let target = if f.op == Op::Load {
                f.arg(1)
            } else {
                Operand::Empty
            };
            return self.load_result(f.op, target, false);
        }
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
                abandoned: 0,
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
            outer_calls: self.load_calls,
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
            stmt: self.cur,
        })
    }

    /// The table's block has ended: its names stay, and the operator gets True. A mutex its
    /// load-time code still holds stays held until the outermost evaluation ends (G2, step B5).
    /// Nothing here can be put back: `Load`'s target was made ready at the operator (B4-1), and
    /// `LoadTable`'s parameter is a step of its own, pushed here once the load has ended (B4-12).
    pub(crate) fn finish_load(&mut self, l: Loading) -> Result<()> {
        // The names its load-time code put in packages are bound now (G1, step B5), while the
        // `Loading` frame still stands: a limit here fails the table whole and traces it, as any
        // limit in its load does (the review of B5, B5-R2).
        self.bind_names(Some(l.table))?;
        self.pop_frame();
        self.pop_owner(false);
        self.run.depth = l.depth;
        self.cur = l.ret;
        self.trace(&Trace::Loaded(Ok(l.table)));
        if l.op == Op::LoadTable {
            // The table is in the namespace: whatever the parameter's store meets now, a limit
            // included, is not the load's (§19.6.78; the ruling on the review's point 6).
            return self.push_frame(Frame::Parameter(Parameter {
                args: l.args,
                root: l.root,
            }));
        }
        let [target, _] = l.args;
        self.load_result(l.op, target, true)
    }

    /// `LoadTable`'s parameter, the step a [`Parameter`] frame takes once the load has ended:
    /// `ParameterData` stored into the object `ParameterPath` names, then the operator's True
    /// given. The node is found and made ready before anything changes, so that a `Pending` puts
    /// this step back with nothing done (`pci.rs`); an object that does not exist is an error, and
    /// the table stays loaded (B4-12).
    pub(crate) fn store_parameter(&mut self, p: Parameter) -> Result<()> {
        let [path, data] = p.args;
        let node = self.parameter_node(path, p.root)?;
        if let Some(node) = node {
            self.load_ready(Loc::Node(node))?;
        }
        self.pop_frame();
        if let Some(node) = node {
            let Operand::Val(data) = data else {
                return err(ErrorKind::Grammar);
            };
            let stored = self.store(Loc::Node(node), data, Mode::Store);
            self.no_retry(stored)?;
        }
        self.load_result(Op::LoadTable, Operand::Empty, true)
    }

    /// The object `LoadTable`'s `ParameterPath` names, if it is not empty: as written if it
    /// begins with `\` or `^`, else under the root path `root`. An object that does not exist is
    /// an error.
    fn parameter_node(&mut self, path: Operand, root: NodeId) -> Result<Option<NodeId>> {
        let Some(path) = self.path_operand(path)? else {
            return Ok(None);
        };
        let node = if path.root() || path.parents() > 0 {
            self.lookup_from(self.scope(), &path)?
        } else {
            self.lookup_exact(root, &path)?
        };
        Ok(Some(node))
    }

    /// Makes a store into `target` ready: its PCI function found first if it needs one, by a
    /// step put back (`pci.rs`). A test may plant its absence (`SkipLoadReadiness`).
    fn load_ready(&mut self, target: Loc) -> Result<()> {
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::SkipLoadReadiness) {
            return Ok(());
        }
        self.target_ready(target)
    }

    /// A store at a load's end that still asks to be put back: the interpreter's own error, never
    /// a retry, since that end has already changed what a put-back step cannot undo (B4-1).
    fn no_retry<T>(&mut self, r: Result<T>) -> Result<T> {
        match r {
            Err(e) if e.kind == ErrorKind::Pending => {
                self.pending = None;
                err(ErrorKind::Grammar)
            }
            other => other,
        }
    }

    /// Whether the code running is the table level of a table being loaded: the nearest of a
    /// method's call and a [`Loading`] frame is the load's, which is so exactly while there are as
    /// many activations as when that load began. A method called from it, or a function walk's,
    /// has one more.
    fn table_level_of_a_load(&self) -> bool {
        self.load_calls == self.calls.len()
    }

    /// Whether a `Return` would leave a table being loaded: its table-level code has no method to
    /// return from, and returning from the method around the `Load` would skip the load's end; it
    /// is `Grammar`, as a `Return` outside any method is.
    pub(crate) fn return_crosses_load(&self) -> bool {
        self.table_level_of_a_load()
    }

    /// Whether `LocalX` and `ArgX` are refused here: at a loaded table's table level they would
    /// be the method's that ran `Load`, which the table has no right to read or write (the
    /// review of B4's addendum, B4-13). The same block loaded by the host has no method, and
    /// `decode` refuses them there with the same kind, `OutsideMethod` (step B5). A test may plant
    /// their admission (`AllowLoadLocals`).
    pub(crate) fn at_loaded_table_level(&self) -> bool {
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::AllowLoadLocals) {
            return false;
        }
        self.table_level_of_a_load()
    }

    /// The nearest load an error `e` inside the evaluation above `base` fails: the top
    /// [`Loading`] frame, unless `e` is a limit or a `Fatal`, which end the evaluation whatever
    /// it was in. `Fatal` asks the OS to log the error and shut down (§19.6.47), so no more
    /// firmware code runs in that evaluation, a loaded table's included (the coordinating
    /// session's ruling on the review of B4, B4-2, clarifying Q3).
    pub(crate) fn failing_load(&self, base: usize, e: &Error) -> Option<usize> {
        if self.loading == 0
            || matches!(
                e.kind,
                ErrorKind::Limit(_) | ErrorKind::Pending | ErrorKind::Fatal { .. }
            )
        {
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
        // What the table's code took goes with the table, each release reported: the evaluation
        // goes on, so this is an end other than its own (G2; the review of B5, B5-R9). Before the
        // unwind, which removes the table's names, so that a mutex or a method the table declared
        // is named in its report (the review of B5's answers, B5-R12). Planted
        // (`ReleaseAfterUnwind`), after it.
        #[cfg(test)]
        let late = crate::tests::planted(crate::tests::Fault::ReleaseAfterUnwind);
        #[cfg(not(test))]
        let late = false;
        if !late {
            self.release_reported(l.held);
        }
        self.unwind(at, l.calls, l.owners);
        if late {
            self.release_reported(l.held);
        }
        self.pending = None;
        self.run.depth = l.depth;
        self.fail_table(l.table);
        self.cur = l.ret;
        self.trace_limit(&e);
        self.trace(&Trace::Loaded(Err(e)));
        // A run-time load binds its names at its end, failed or not: what the table stored in an
        // object older than it is still read (the review of B5's answers, B5-R10). After the
        // load's `Loaded`, so a limit here is the evaluation's, which aborts it.
        #[cfg(test)]
        let skip = crate::tests::planted(crate::tests::Fault::NoFailedPass);
        #[cfg(not(test))]
        let skip = false;
        if !skip {
            self.bind_names(Some(l.table))?;
        }
        let [target, _] = l.args;
        self.load_result(l.op, target, false)
    }

    /// Marks table `index` failed. The names it put in packages stay listed: a boot table's are
    /// bound by [`Interpreter::bind_deferred`], a run-time load's at its end (`fail_load`); those
    /// of a load an aborted evaluation failed are bound by `bind_deferred` if it has yet to run,
    /// and are otherwise looked up when read (B5-R10). A listed name is read from the table's
    /// bytes, so they go only once none is listed, still charged to the memory limit until then
    /// (the review of B5's second addendum, B5-R15; `release_failed_bytes`). Planted
    /// (`ReleaseBeforeBinding`), they go at once, as before.
    pub(crate) fn fail_table(&mut self, index: u32) {
        if let Some(t) = self.tables.get_mut(us(index)) {
            t.failed = true;
            #[cfg(test)]
            if crate::tests::planted(crate::tests::Fault::ReleaseBeforeBinding) {
                let aml = core::mem::take(&mut t.aml);
                mem::release(aml, &mut self.budget);
            }
        }
        self.release_failed_bytes();
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
            let stored = self.store(l, v, Mode::Store);
            self.no_retry(stored)?;
        }
        self.give_val(v)
    }
}
