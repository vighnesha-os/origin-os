//! The engine: one pass over the AML, with its own stack of frames on the heap and no Rust
//! recursion (the contract's §4).
//!
//! - A **block** frame runs a term list to its end: a table, a scope, a method body, an `If`, an
//!   `Else` or a `While`.
//! - An **operand** frame holds an opcode while its operands are decoded, by the kinds `op.rs`
//!   gives; when the last arrives the operator runs (`exec.rs`) and its value goes to the frame
//!   below.
//! - A **call** frame marks a method activation; its locals and arguments are in `calls`.
//!
//! Loading a table and running a method are the same loop. Every step counts against the limits;
//! the collector runs only between steps, when nothing is held outside the frames.

use alloc::vec::Vec;

use crate::config::DefinitionBlock;
use crate::error::{At, Error, ErrorKind, Limit, Result, err};
use crate::host::{Host, Trace};
use crate::mem;
use crate::name::{self, AmlName, InTable};
use crate::namespace::{Namespace, Owner};
use crate::object::{NodeId, ObjRef, Object, RefTarget, ScopeKind, Val};
use crate::op::{self, Class, Kind, Op};
use crate::{INTERPRETER_REVISION, Interpreter, Table};

/// Where execution is: a table and an offset in its term list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) table: u32,
    pub(crate) pc: u32,
}

/// A place a value can be stored or looked at (`SuperName`, `Target`, `SimpleName`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Loc {
    /// The null name: the value is discarded.
    Null,
    /// The `Debug` object.
    Debug,
    Local(u8),
    Arg(u8),
    Node(NodeId),
    /// Through an object reference.
    Ref(RefTarget),
    /// `CondRefOf`'s operand that names nothing.
    Missing,
}

/// A decoded operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operand {
    Empty,
    Val(Val),
    Loc(Loc),
    Name(AmlName),
    Raw(u64),
}

/// An opcode collecting its operands.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OpFrame {
    pub(crate) op: Op,
    /// Where the opcode is.
    pub(crate) at: u32,
    /// The end of its package, or 0.
    pub(crate) end: u32,
    pub(crate) kinds: &'static [Kind],
    pub(crate) arity: u8,
    pub(crate) n: u8,
    pub(crate) args: [Operand; 7],
    /// 0 while the operands are collected; 1 while a package's elements are.
    pub(crate) phase: u8,
    /// Per opcode: a `While`'s predicate offset; a package's next element.
    pub(crate) aux: u64,
    /// The method a `Call` runs.
    pub(crate) node: NodeId,
}

impl OpFrame {
    pub(crate) fn arg(&self, i: usize) -> Operand {
        self.args.get(i).copied().unwrap_or(Operand::Empty)
    }
}

/// What a block runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockKind {
    Table,
    Scope,
    Method,
    If,
    Else,
    While,
}

/// A frame. Kept unboxed on purpose: the frames live in one vector that `mem.rs` grows, and a box
/// per operand frame would be an allocation the allocation discipline does not allow.
#[derive(Clone, Copy, Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Frame {
    Block {
        kind: BlockKind,
        end: u32,
        scope: NodeId,
        /// A `While`'s predicate offset.
        pred: u32,
        /// Where the statement it is running began: what load-time code abandons (step B5).
        stmt: Cursor,
    },
    Op(OpFrame),
    Call,
    /// A PCI_Config region's function being found (`pci.rs`).
    Resolve(crate::pci::Resolve),
    /// A table being loaded by `Load` or `LoadTable`, below its table block until that ends
    /// (`load.rs`).
    Loading(crate::load::Loading),
    /// `LoadTable`'s parameter, stored once its table has loaded (`load.rs`).
    Parameter(crate::load::Parameter),
}

/// The units of work between two readings of the clock.
const CLOCK_UNITS: u64 = 1_024;

/// What a frame sees around it: the scope names resolve in (the innermost block's) and the end of
/// the innermost block or package (`None`: the table's end). Kept beside each frame, made from the
/// one below when the frame is pushed: a frame's own scope and end never change once pushed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ctx {
    scope: NodeId,
    end: Option<u32>,
}

/// A method activation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CallFrame {
    pub(crate) serial: u64,
    /// The method running, for a report naming what took a mutex (the review of B5, B5-R8).
    pub(crate) method: NodeId,
    pub(crate) ret: Cursor,
    pub(crate) locals: [Val; 8],
    pub(crate) args: [Val; 7],
    /// A serialized method: its implicit mutex is held while it runs (§19.6.85).
    pub(crate) serialized: bool,
}

/// Where an evaluation began: what [`Interpreter::abort`] goes back to, and no further (the
/// review of B1, F14).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Marks {
    pub(crate) frames: usize,
    pub(crate) calls: usize,
    pub(crate) owners: usize,
    pub(crate) held: usize,
}

/// Who is creating names now, and what it has created.
#[derive(Debug)]
pub(crate) struct OwnerCtx {
    pub(crate) owner: Owner,
    pub(crate) created: Vec<NodeId>,
}

/// What a term is decoded for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want {
    /// A term in a term list: its value, if any, is discarded.
    Statement,
    /// A value (`TermArg`); `raw` keeps an argument's reference.
    Value { raw: bool },
    /// `DefName`'s data object.
    Data,
    /// A package element: a data object or a name kept unresolved.
    Element,
    /// A place.
    Location(Kind),
}

/// The operands a method invocation takes: up to seven `TermArg`s.
pub(crate) const CALL_KINDS: &[Kind] = &[Kind::TermArg; 7];

pub(crate) const ELSE: u8 = 0xA1;

fn us(x: u32) -> usize {
    usize::try_from(x).unwrap_or(usize::MAX)
}

fn add(a: u32, b: u32) -> Result<u32> {
    a.checked_add(b)
        .ok_or_else(|| Error::from(ErrorKind::Truncated))
}

impl<H: Host> Interpreter<H> {
    // --- Construction ------------------------------------------------------------------------

    /// An interpreter with an empty namespace holding the predefined scopes and objects
    /// (ACPI 6.6 Tables 5.220 and 5.246). `width` is what the DSDT's revision sets, read by the
    /// caller from its checked header.
    ///
    /// # Errors
    /// [`Limit::Memory`] if the limit cannot hold the predefined objects.
    pub fn new(host: H, config: crate::Config, width: crate::IntegerWidth) -> Result<Self> {
        let mut budget = crate::mem::Budget::new(config.limits.memory_bytes);
        let mut heap = crate::object::Heap::new();
        let root_obj = heap.alloc(
            Object::Scope {
                kind: ScopeKind::Typeless,
                node: Namespace::root(),
            },
            &mut budget,
        )?;
        let ns = Namespace::new(root_obj, &mut budget)?;
        let mut me = Self {
            host,
            config,
            width,
            budget,
            heap,
            ns,
            tables: Vec::new(),
            frames: Vec::new(),
            ctx: Vec::new(),
            calls: Vec::new(),
            owners: Vec::new(),
            cur: Cursor { table: 0, pc: 0 },
            serial: 0,
            held: Vec::new(),
            deferred: Vec::new(),
            nested: 0,
            pending: None,
            resolving: 0,
            loading: 0,
            load_calls: usize::MAX,
            global_depth: 0,
            run: crate::RunCounts::default(),
            base: 0,
            result: Val::None,
            gc_threshold: 0,
            steps_done: 0,
        };
        me.gc_threshold = me.budget.limit() / 2;
        me.predefine()?;
        Ok(me)
    }

    fn predefine(&mut self) -> Result<()> {
        let mut list = Vec::new();
        let root = Namespace::root();
        for seg in [*b"_GPE", *b"_PR_", *b"_SB_", *b"_SI_", *b"_TZ_"] {
            let obj = self.heap.alloc(Object::Uninitialized, &mut self.budget)?;
            let node =
                self.ns
                    .create(root, seg, obj, Owner::Builtin, &mut list, &mut self.budget)?;
            self.heap.replace(
                obj,
                Object::Scope {
                    kind: ScopeKind::Typeless,
                    node,
                },
                &mut self.budget,
            )?;
        }
        let gl = self.heap.alloc(
            Object::Mutex(crate::object::Mutex {
                sync: 0,
                depth: 0,
                global: true,
            }),
            &mut self.budget,
        )?;
        self.ns.create(
            root,
            *b"_GL_",
            gl,
            Owner::Builtin,
            &mut list,
            &mut self.budget,
        )?;
        let osi = self.heap.alloc(Object::Osi, &mut self.budget)?;
        self.ns.create(
            root,
            *b"_OSI",
            osi,
            Owner::Builtin,
            &mut list,
            &mut self.budget,
        )?;
        let os = mem::copy_bytes(crate::osi::OS_NAME, &mut self.budget)?;
        let os = self.heap.alloc(Object::String(os), &mut self.budget)?;
        self.ns.create(
            root,
            *b"_OS_",
            os,
            Owner::Builtin,
            &mut list,
            &mut self.budget,
        )?;
        let rev = self
            .heap
            .alloc(Object::Integer(crate::osi::REV), &mut self.budget)?;
        self.ns.create(
            root,
            *b"_REV",
            rev,
            Owner::Builtin,
            &mut list,
            &mut self.budget,
        )?;
        mem::release(list, &mut self.budget);
        Ok(())
    }

    // --- Loading -----------------------------------------------------------------------------

    /// Loads a definition block: its term list is run at the root (ACPI 6.6 §5.4.2), on a
    /// best-effort basis (step B5; TASKS 64's ledger §2): a statement that fails is abandoned
    /// and reported, and the load goes on (`tolerate.rs`). On a failure the parser cannot step
    /// past, a limit or `Fatal`, every name it created is removed, and its methods are never run.
    /// A block identical to a table already loaded is refused, nothing of it created.
    ///
    /// # Errors
    /// What fails the table whole, with where; a limit reached; [`ErrorKind::Duplicate`] for a
    /// block identical to a table already loaded ([`crate::Report::AlreadyLoaded`] tells which).
    pub fn load(&mut self, block: DefinitionBlock<'_>) -> Result<u32> {
        if let Some(table) =
            self.identical_table(block.signature, block.oem_id, block.oem_table_id, block.aml)
        {
            self.begin_evaluation()?;
            self.tell(&crate::Report::AlreadyLoaded { table });
            self.end_evaluation();
            return err(ErrorKind::Duplicate);
        }
        let index =
            u32::try_from(self.tables.len()).map_err(|_| ErrorKind::Limit(Limit::Memory))?;
        let end = u32::try_from(block.aml.len()).map_err(|_| ErrorKind::Truncated)?;
        mem::reserve(&mut self.tables, 1, &mut self.budget)?;
        let aml = mem::copy_bytes(block.aml, &mut self.budget)?;
        // Cannot fail: the room was reserved before the copy (the review of B1, F2).
        mem::push(
            &mut self.tables,
            Table {
                aml,
                signature: block.signature,
                oem_id: block.oem_id,
                oem_table_id: block.oem_table_id,
                failed: false,
                abandoned: 0,
            },
            &mut self.budget,
        )?;
        self.trace(&Trace::Load {
            signature: block.signature,
            oem_table_id: block.oem_table_id,
        });
        let marks = self.begin_evaluation()?;
        let start = self.push_owner(Owner::Table(index));
        let outcome = start.and_then(|()| {
            self.cur = Cursor {
                table: index,
                pc: 0,
            };
            self.push_frame(Frame::Block {
                kind: BlockKind::Table,
                end,
                scope: Namespace::root(),
                pred: 0,
                stmt: self.cur,
            })?;
            self.run_frames(marks.frames)?;
            // What load-time code still holds is released and reported (G2, step B5).
            self.release_reported(marks.held);
            Ok(())
        });
        match outcome {
            Ok(()) => {
                self.pop_owner(false);
                self.end_evaluation();
                self.trace(&Trace::Loaded(Ok(index)));
                Ok(index)
            }
            Err(e) => {
                // The table's identity stays; its bytes go (the review of B1, F7). Its methods
                // already fail `TableFailed`, and a name kept in a package pointing into it now
                // fails `Truncated`.
                self.fail_table(index);
                self.abort(marks, &e);
                self.end_evaluation();
                self.trace(&Trace::Loaded(Err(e)));
                Err(e)
            }
        }
    }

    /// Tells a tracing host of the limit `e` reached, if it is one.
    pub(crate) fn trace_limit(&mut self, e: &Error) {
        if let ErrorKind::Limit(l) = e.kind {
            self.trace(&Trace::Limit(l));
        }
    }

    // --- Evaluations -------------------------------------------------------------------------

    /// Starts a top-level evaluation: its counts, and the marks it runs above.
    pub(crate) fn begin_evaluation(&mut self) -> Result<Marks> {
        self.run = crate::RunCounts {
            started: self.host.now_100ns(),
            charged_seen: self.budget.charged(),
            ..crate::RunCounts::default()
        };
        let _ = self.ns.take_work();
        self.result = Val::None;
        self.base = self.frames.len();
        Ok(Marks {
            frames: self.base,
            calls: self.calls.len(),
            owners: self.owners.len(),
            held: self.held.len(),
        })
    }

    /// Ends one: everything unreachable is collected, and the stacks, empty at the top level,
    /// give their memory back.
    pub(crate) fn end_evaluation(&mut self) {
        self.tell_suppressed();
        self.result = Val::None;
        // At the top level nothing may stay held: the paths above release and report; this only
        // makes sure no hold, the Global Lock's least, outlives the evaluation whatever path
        // ended it (the review of B2, B2-6).
        if self.calls.is_empty() {
            self.release_held(0);
            #[cfg(test)]
            let planted = crate::tests::planted(crate::tests::Fault::KeepGlobalLock);
            #[cfg(not(test))]
            let planted = false;
            if self.global_depth > 0 && !planted {
                self.global_depth = 0;
                let _ = self.host.release_global_lock();
            }
        }
        // Collected when due, as between steps, not after every evaluation: an evaluation that
        // reads one name must not cost a pass over the whole heap (the review of B2, B2-7). The
        // work is added to the evaluation's count, which the initialization walk sums.
        let collected = self.collect_if_due();
        if let Some(work) = collected {
            self.run.steps = self.run.steps.saturating_add(work);
        }
        self.steps_done = self.steps_done.saturating_add(self.run.steps);
        if self.frames.is_empty() {
            mem::release(core::mem::take(&mut self.frames), &mut self.budget);
            mem::release(core::mem::take(&mut self.ctx), &mut self.budget);
        }
        if self.calls.is_empty() {
            mem::release(core::mem::take(&mut self.calls), &mut self.budget);
        }
        if self.owners.is_empty() {
            mem::release(core::mem::take(&mut self.owners), &mut self.budget);
        }
        // The count is made afresh with each collection, which reads the heap anyway: a charge a
        // failure path left uncredited (the review of B1, F2) raises the count, which brings the
        // next collection, and its recount, nearer. Not after every evaluation, for the same
        // reason as the collection.
        if collected.is_some() {
            self.recount();
        }
    }

    /// The collection and recount that would come when due, now: for tests that measure what
    /// is held.
    #[cfg(test)]
    pub(crate) fn settle_memory(&mut self) {
        self.collect_garbage();
        self.recount();
    }

    /// Counts afresh every byte the interpreter holds and sets the budget to it: what was
    /// charged and not credited on a failure path is given back (the review of B1, F2).
    pub(crate) fn recount(&mut self) {
        use core::mem::size_of;
        let mut n = self.heap.bytes().saturating_add(self.ns.bytes());
        n = n.saturating_add(self.tables.capacity().saturating_mul(size_of::<Table>()));
        for t in &self.tables {
            n = n.saturating_add(t.aml.capacity());
        }
        n = n
            .saturating_add(self.frames.capacity().saturating_mul(size_of::<Frame>()))
            .saturating_add(self.ctx.capacity().saturating_mul(size_of::<Ctx>()))
            .saturating_add(self.calls.capacity().saturating_mul(size_of::<CallFrame>()))
            .saturating_add(self.owners.capacity().saturating_mul(size_of::<OwnerCtx>()))
            .saturating_add(
                self.held
                    .capacity()
                    .saturating_mul(size_of::<crate::sync::Held>()),
            )
            .saturating_add(
                self.deferred
                    .capacity()
                    .saturating_mul(size_of::<crate::object::ObjRef>()),
            );
        for o in &self.owners {
            n = n.saturating_add(o.created.capacity().saturating_mul(size_of::<NodeId>()));
        }
        #[cfg(test)]
        crate::tests::note_drift(self.budget.used(), n);
        self.budget.recount(n);
    }

    /// Undoes everything above `marks` after an error: frames, activations, the names their
    /// creators made (a table being loaded included: that is the rollback), and the mutexes taken.
    /// What was there when the evaluation began stays, so that one evaluation inside another
    /// would leave the outer one whole (the review of B1, F14). Today every evaluation is
    /// top-level, and every mark is zero.
    ///
    /// A tracing host is told, in this order: each activation the unwind leaves; the limit, if
    /// `e` is one; then a failed `Loaded` for each table `Load` or `LoadTable` was loading, so that
    /// no `Load` goes without its `Loaded`. Those tables fail like any other: their names go with
    /// their creators in the unwind, their bytes in `fail_tables_loading` (`load.rs`).
    pub(crate) fn abort(&mut self, marks: Marks, e: &Error) {
        let tables = self.fail_tables_loading(marks.frames);
        self.unwind(marks.frames, marks.calls, marks.owners);
        self.release_held(marks.held);
        self.result = Val::None;
        self.trace_limit(e);
        for _ in 0..tables {
            self.trace(&Trace::Loaded(Err(*e)));
        }
    }

    /// Pops frames, activations and creators above the given marks; the names those creators
    /// made are removed.
    pub(crate) fn unwind(&mut self, frames: usize, calls: usize, owners: usize) {
        self.truncate_frames(frames);
        for _ in calls..self.calls.len() {
            self.trace(&Trace::Leave);
        }
        self.calls.truncate(calls);
        while self.owners.len() > owners {
            if let Some(ctx) = self.owners.pop() {
                #[cfg(test)]
                let planted = crate::tests::planted(crate::tests::Fault::SkipRollback)
                    && matches!(ctx.owner, Owner::Table(_));
                #[cfg(not(test))]
                let planted = false;
                if !planted {
                    self.ns.remove_list(&ctx.created);
                }
                mem::release(ctx.created, &mut self.budget);
            }
        }
    }

    pub(crate) fn push_owner(&mut self, owner: Owner) -> Result<()> {
        mem::push(
            &mut self.owners,
            OwnerCtx {
                owner,
                created: Vec::new(),
            },
            &mut self.budget,
        )
    }

    /// Ends the newest creator: a method's names are removed (`remove`), a table's kept.
    pub(crate) fn pop_owner(&mut self, remove: bool) {
        if let Some(ctx) = self.owners.pop() {
            #[cfg(test)]
            let remove = remove && !crate::tests::planted(crate::tests::Fault::KeepDynamicNames);
            if remove {
                self.ns.remove_list(&ctx.created);
            }
            mem::release(ctx.created, &mut self.budget);
        }
    }

    pub(crate) fn push_frame(&mut self, f: Frame) -> Result<()> {
        let below = self.ctx.last().copied().unwrap_or(Ctx {
            scope: Namespace::root(),
            end: None,
        });
        let here = match &f {
            Frame::Block { end, scope, .. } => Ctx {
                scope: *scope,
                end: Some(*end),
            },
            Frame::Op(o) if o.end != 0 => Ctx {
                scope: below.scope,
                end: Some(o.end),
            },
            _ => below,
        };
        // Room for both first, so that the two stacks never differ in length.
        mem::reserve(&mut self.ctx, 1, &mut self.budget)?;
        let resolve = matches!(f, Frame::Resolve(_));
        let loading = match &f {
            Frame::Loading(l) => Some(l.calls_mark()),
            _ => None,
        };
        mem::push(&mut self.frames, f, &mut self.budget)?;
        mem::push(&mut self.ctx, here, &mut self.budget)?;
        if let Some(calls) = loading {
            self.loading = self.loading.saturating_add(1);
            self.load_calls = calls;
        }
        // Counted where every frame is pushed, a step put back with a `Resolve` on top among
        // them, as `pop_frame` and `truncate_frames` count them out (the review of B2's Addendum
        // 2, N2-1).
        if resolve {
            self.resolving = self.resolving.saturating_add(1);
        }
        Ok(())
    }

    /// Pops the top frame, with what it saw.
    pub(crate) fn pop_frame(&mut self) -> Option<Frame> {
        let f = self.frames.pop();
        if f.is_some() {
            self.ctx.pop();
        }
        if matches!(f, Some(Frame::Resolve(_))) {
            self.resolving = self.resolving.saturating_sub(1);
        }
        if let Some(Frame::Loading(l)) = &f {
            self.loading = self.loading.saturating_sub(1);
            self.load_calls = l.outer_calls();
        }
        f
    }

    /// Pops frames down to `n`. The `Resolve` frames among them are counted out, so that the
    /// count stays true when an error unwinds them (`pci.rs`).
    pub(crate) fn truncate_frames(&mut self, n: usize) {
        let (gone, loads) = self.frames.get(n..).map_or((0, 0), |f| {
            (
                f.iter().filter(|f| matches!(f, Frame::Resolve(_))).count(),
                f.iter().filter(|f| matches!(f, Frame::Loading(_))).count(),
            )
        });
        self.resolving = self
            .resolving
            .saturating_sub(u32::try_from(gone).unwrap_or(u32::MAX));
        self.loading = self
            .loading
            .saturating_sub(u32::try_from(loads).unwrap_or(u32::MAX));
        // The lowest load popped was inside whichever is innermost now.
        if let Some(Frame::Loading(l)) = self
            .frames
            .get(n..)
            .and_then(|f| f.iter().find(|f| matches!(f, Frame::Loading(_))))
        {
            self.load_calls = l.outer_calls();
        }
        self.frames.truncate(n);
        self.ctx.truncate(n);
    }

    /// Runs until the frames are back to `base`. A statement's failure in load-time code
    /// abandons that statement alone, and the load goes on (`tolerate.rs`; step B5). Any other
    /// error inside a table that `Load` or `LoadTable` is loading, other than a limit or `Fatal`,
    /// fails that load alone: it is rolled back and its operator gives Zero, and the evaluation
    /// goes on (`load.rs`; B4's contract, Q3).
    pub(crate) fn run_frames(&mut self, base: usize) -> Result<()> {
        loop {
            if self.frames.len() <= base {
                return Ok(());
            }
            let mut failure = self.step().err();
            while let Some(mut e) = failure.take() {
                if e.at.is_none() {
                    e.at = Some(At {
                        table: self.cur.table,
                        offset: self.cur.pc,
                    });
                }
                if let Some(b) = self.abandon_target(base, &e) {
                    failure = self.abandon(b, e).err();
                    continue;
                }
                match self.failing_load(base, &e) {
                    Some(at) => failure = self.fail_load_at(at, e).err(),
                    None => return Err(e),
                }
            }
        }
    }

    /// One step: an opcode, with the work done since the last count (the review of B1, F1).
    fn tick(&mut self) -> Result<()> {
        self.charge(1)?;
        if self.nested == 0
            && let Some(work) = self.collect_if_due()
        {
            self.charge(work)?;
        }
        Ok(())
    }

    /// Collects if what is held has passed the threshold, and sets the next: when half the room
    /// left after this collection is used, and never before 64 KiB more is, so collections are
    /// spaced, never one a step nor one an evaluation (the review of B1, F1, and of B2, B2-7).
    /// Returns the work done if it collected: a unit for each eight slots, each root, and each
    /// object and package element marking read.
    fn collect_if_due(&mut self) -> Option<u64> {
        if self.budget.used() <= self.gc_threshold {
            return None;
        }
        let slots = u64::try_from(self.heap.slot_count()).unwrap_or(u64::MAX);
        // Besides the slots, the collector reads every frame, activation and hold as roots, and
        // the recount that follows every table and creator: a unit each.
        let roots = [
            self.frames.len(),
            self.calls.len(),
            self.held.len(),
            self.deferred.len(),
            self.tables.len(),
            self.owners.len(),
        ]
        .iter()
        .fold(0u64, |n, &k| {
            n.saturating_add(u64::try_from(k).unwrap_or(u64::MAX))
        });
        self.collect_garbage();
        let used = self.budget.used();
        let gap = (self.budget.limit().saturating_sub(used) / 2).max(64 << 10);
        self.gc_threshold = used.saturating_add(gap).min(self.budget.limit());
        // Marking reads every element of every live package, however few the slots: a unit each.
        let traced = self.heap.take_traced();
        Some((slots / 8).saturating_add(roots).saturating_add(traced))
    }

    /// Counts `units` of work against the step limit, with the work done since the last count:
    /// each sibling compared in the namespace, and a unit for each 64 bytes allocated or moved
    /// (what is copied, converted or filled is allocated first; a reallocation counts what it
    /// copies). The clock is read whenever 1,024 units have been counted since it was last read,
    /// so that between two readings there is at most that much work and the operation being
    /// charged, itself charged before it runs where it is large (the review of B1, F1 and A1).
    pub(crate) fn charge(&mut self, units: u64) -> Result<()> {
        let compared = self.ns.take_work();
        let charged = self.budget.charged();
        let bytes = charged.saturating_sub(self.run.charged_seen);
        self.run.charged_seen = charged;
        let all = units.saturating_add(compared).saturating_add(bytes / 64);
        self.run.steps = self.run.steps.saturating_add(all);
        if self.run.steps > self.config.limits.steps {
            return err(ErrorKind::Limit(Limit::Steps));
        }
        self.run.since_clock = self.run.since_clock.saturating_add(all);
        if self.run.since_clock >= CLOCK_UNITS {
            self.run.since_clock = 0;
            self.check_time()?;
        }
        Ok(())
    }

    /// Counts the work done since the last count (see [`Interpreter::charge`]).
    pub(crate) fn charge_pending(&mut self) -> Result<()> {
        self.charge(0)
    }

    /// Notes `n` units of work done where nothing can be charged at once (a `&self` method, or
    /// a loop that must finish what it started): the next [`Interpreter::charge`] counts them.
    pub(crate) fn note_work(&self, n: u64) {
        self.ns.note_work(n);
    }

    pub(crate) fn check_time(&mut self) -> Result<()> {
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::NoTimeLimit) {
            return Ok(());
        }
        let now = self.host.now_100ns();
        if now.saturating_sub(self.run.started) > self.config.limits.time_100ns {
            return err(ErrorKind::Limit(Limit::Time));
        }
        Ok(())
    }

    /// One step.
    fn step(&mut self) -> Result<()> {
        self.tick()?;
        let Some(top) = self.frames.last().copied() else {
            return Ok(());
        };
        let before = (
            self.frames.len(),
            self.calls.len(),
            self.owners.len(),
            self.cur,
            self.run.depth,
        );
        match self.step_on(top) {
            // An access wants a PCI_Config region's function first (`pci.rs`). Nothing the step
            // did reached the host: it is put back as it was, the function is found by frames
            // above it, and then it runs again (the review of B2, B2-1).
            Err(e) if e.kind == ErrorKind::Pending => {
                let region = self.pending.take().ok_or(ErrorKind::Grammar)?;
                let (frames, calls, owners, cur, depth) = before;
                self.unwind(frames.saturating_sub(1), calls, owners);
                self.push_frame(top)?;
                self.cur = cur;
                self.run.depth = depth;
                self.start_resolve(region)
            }
            other => other,
        }
    }

    fn step_on(&mut self, top: Frame) -> Result<()> {
        match top {
            Frame::Block { end, .. } => {
                if self.cur.pc == end {
                    return self.end_block();
                }
                if self.cur.pc > end {
                    return err(ErrorKind::PackageLength);
                }
                if let Some(Frame::Block { stmt, .. }) = self.frames.last_mut() {
                    *stmt = self.cur;
                }
                self.decode(Want::Statement)
            }
            Frame::Op(f) => {
                if f.phase == 0 && f.n < f.arity {
                    self.operand(&f)
                } else if f.phase == 2 {
                    self.pop_frame();
                    self.place_element(f)
                } else if f.phase == 1 || matches!(f.op, Op::Package | Op::VarPackage) {
                    self.package_step(f)
                } else {
                    self.pop_frame();
                    self.complete(f).map_err(|mut e| {
                        if e.at.is_none() {
                            e.at = Some(At {
                                table: self.cur.table,
                                offset: f.at,
                            });
                        }
                        e
                    })
                }
            }
            // A call frame is never on top: its method's block is above it until it returns.
            Frame::Call => err(ErrorKind::Grammar),
            Frame::Resolve(r) => self.resolve_step(r),
            // A loaded table's block has ended: the load is complete (`load.rs`).
            Frame::Loading(l) => self.finish_load(l),
            // `LoadTable`'s parameter, after its table has loaded (`load.rs`).
            Frame::Parameter(p) => self.store_parameter(p),
        }
    }

    // --- Reading the AML ---------------------------------------------------------------------

    pub(crate) fn aml(&self) -> &[u8] {
        self.tables
            .get(us(self.cur.table))
            .map_or(&[], |t| t.aml.as_slice())
    }

    pub(crate) fn byte(&self, at: u32) -> Result<u8> {
        self.aml()
            .get(us(at))
            .copied()
            .ok_or_else(|| Error::from(ErrorKind::Truncated))
    }

    fn le(&self, at: u32, bytes: u32) -> Result<u64> {
        let mut v: u64 = 0;
        for k in (0..bytes).rev() {
            v = (v << 8) | u64::from(self.byte(add(at, k)?)?);
        }
        Ok(v)
    }

    /// A package length at `at` (§20.2.4): its value and the offset after it.
    pub(crate) fn pkg_length(&self, at: u32) -> Result<(u32, u32)> {
        let lead = self.byte(at)?;
        let follow = u32::from(lead >> 6);
        if follow == 0 {
            return Ok((u32::from(lead & 0x3F), add(at, 1)?));
        }
        if lead & 0x30 != 0 {
            return err(ErrorKind::PackageLength);
        }
        let mut v = u32::from(lead & 0x0F);
        for (k, shift) in (0..follow).zip([4u32, 12, 20]) {
            let b = u32::from(self.byte(add(at, add(k, 1)?)?)?);
            v |= b.checked_shl(shift).unwrap_or(0);
        }
        Ok((v, add(at, add(follow, 1)?)?))
    }

    /// The end of the innermost construct being decoded: a block's or a package's.
    pub(crate) fn enclosing_end(&self) -> u32 {
        match self.ctx.last() {
            Some(Ctx { end: Some(end), .. }) => *end,
            _ => u32::try_from(self.aml().len()).unwrap_or(u32::MAX),
        }
    }

    /// The scope names are resolved in and created under: the innermost block's.
    pub(crate) fn scope(&self) -> NodeId {
        self.ctx.last().map_or(Namespace::root(), |c| c.scope)
    }

    /// The node an alias object names, for the namespace's lookups.
    pub(crate) fn alias_target(heap: &crate::object::Heap, obj: ObjRef) -> Option<NodeId> {
        match heap.get(obj) {
            Ok(Object::Alias(n)) => Some(*n),
            _ => None,
        }
    }

    /// Resolves a name read from the current table, from the current scope.
    pub(crate) fn resolve(&self, name: AmlName) -> Result<Option<NodeId>> {
        let heap = &self.heap;
        let found = self.ns.lookup(
            self.scope(),
            &InTable {
                name,
                aml: self.aml(),
            },
            |o| Self::alias_target(heap, o),
        )?;
        match found {
            Some(n) => Ok(Some(self.ns.unalias(n, |o| Self::alias_target(heap, o))?)),
            None => Ok(None),
        }
    }

    /// Creates the node a name read from the current table names, holding `obj`.
    pub(crate) fn create(&mut self, name: AmlName, obj: ObjRef) -> Result<NodeId> {
        let scope = self.scope();
        let heap = &self.heap;
        let (parent, seg) = {
            let aml = self
                .tables
                .get(us(self.cur.table))
                .map_or(&[][..], |t| t.aml.as_slice());
            self.ns.place(scope, &InTable { name, aml }, |o| {
                Self::alias_target(heap, o)
            })?
        };
        let owner = self.owners.last().map_or(Owner::Builtin, |c| c.owner);
        let Some(ctx) = self.owners.last_mut() else {
            return err(ErrorKind::Grammar);
        };
        self.ns
            .create(parent, seg, obj, owner, &mut ctx.created, &mut self.budget)
    }

    // --- Decoding ----------------------------------------------------------------------------

    /// Decodes the operand the top frame `f` waits for.
    fn operand(&mut self, f: &OpFrame) -> Result<()> {
        let kind = f
            .kinds
            .get(usize::from(f.n))
            .copied()
            .ok_or(ErrorKind::Grammar)?;
        let pc = self.cur.pc;
        match kind {
            Kind::Byte => {
                let b = self.byte(pc)?;
                self.cur.pc = add(pc, 1)?;
                self.give(Operand::Raw(u64::from(b)))
            }
            Kind::Word => {
                let v = self.le(pc, 2)?;
                self.cur.pc = add(pc, 2)?;
                self.give(Operand::Raw(v))
            }
            Kind::DWord => {
                let v = self.le(pc, 4)?;
                self.cur.pc = add(pc, 4)?;
                self.give(Operand::Raw(v))
            }
            Kind::NameNew | Kind::NameOld | Kind::NameRaw => {
                let (name, next) = name::parse(self.aml(), pc)?;
                self.cur.pc = next;
                self.give(Operand::Name(name))
            }
            Kind::TermArg => self.decode(Want::Value { raw: false }),
            Kind::TermArgRaw => self.decode(Want::Value { raw: true }),
            Kind::Data => self.decode(Want::Data),
            Kind::Super | Kind::SuperOpt | Kind::Target | Kind::Simple => {
                self.decode(Want::Location(kind))
            }
        }
    }

    /// Hands an operand to the top frame. A value for a block is discarded; at the bottom of the
    /// evaluation it is its result.
    fn give(&mut self, operand: Operand) -> Result<()> {
        if self.frames.len() <= self.base {
            if let Operand::Val(v) = operand {
                self.result = v;
            }
            return Ok(());
        }
        // A method run for a PCI_Config region's function hands its value to the walk.
        if let Some(Frame::Resolve(r)) = self.frames.last_mut() {
            return match operand {
                Operand::Val(v) => {
                    r.got = Some(v);
                    Ok(())
                }
                _ => err(ErrorKind::Grammar),
            };
        }
        let Some(Frame::Op(f)) = self.frames.last_mut() else {
            return Ok(());
        };
        if f.phase == 1 {
            return err(ErrorKind::Grammar);
        }
        let kind = f.kinds.get(usize::from(f.n)).copied();
        let operand = match (kind, operand) {
            (
                Some(Kind::Super | Kind::SuperOpt | Kind::Target | Kind::Simple),
                Operand::Val(Val::Ref(t)),
            ) => Operand::Loc(Loc::Ref(t)),
            (Some(Kind::Super | Kind::SuperOpt | Kind::Target | Kind::Simple), Operand::Val(_)) => {
                return err(ErrorKind::Type);
            }
            (Some(Kind::TermArg | Kind::TermArgRaw | Kind::Data), Operand::Val(Val::None)) => {
                return err(ErrorKind::NoValue);
            }
            (_, o) => o,
        };
        let slot = f.args.get_mut(usize::from(f.n)).ok_or(ErrorKind::Grammar)?;
        *slot = operand;
        f.n = f.n.saturating_add(1);
        Ok(())
    }

    /// Hands a value to whatever waits for it.
    pub(crate) fn give_val(&mut self, v: Val) -> Result<()> {
        self.give(Operand::Val(v))
    }

    /// Whether the frame on top waits for a value (rather than being a block).
    fn top_wants_value(&self) -> bool {
        self.frames.len() > self.base
            && matches!(self.frames.last(), Some(Frame::Op(_) | Frame::Resolve(_)))
    }

    /// Decodes one term at the cursor for `want`.
    fn decode(&mut self, want: Want) -> Result<()> {
        let at = self.cur.pc;
        let b = self.byte(at)?;
        let next = add(at, 1)?;
        match b {
            // A table loaded at run time has no locals or arguments of its own at its table level:
            // those are the method's that ran `Load` (the review of B4's addendum, B4-13).
            // So has a boot table's, which has no method at all: the same kind, so that only
            // its statement is abandoned (step B5, Q2's line).
            0x60..=0x6E
                if self.at_loaded_table_level() || (self.loading == 0 && self.calls.is_empty()) =>
            {
                return err(ErrorKind::OutsideMethod);
            }
            0x60..=0x67 => {
                #[cfg(test)]
                crate::tests::ran(u16::from(b));
                self.cur.pc = next;
                let i = b.wrapping_sub(0x60);
                return match want {
                    Want::Statement => Ok(()),
                    Want::Location(_) => self.give(Operand::Loc(Loc::Local(i))),
                    Want::Value { .. } => {
                        let v = self.local(i)?;
                        self.give_val(v)
                    }
                    Want::Data | Want::Element => err(ErrorKind::Grammar),
                };
            }
            0x68..=0x6E => {
                #[cfg(test)]
                crate::tests::ran(u16::from(b));
                self.cur.pc = next;
                let i = b.wrapping_sub(0x68);
                return match want {
                    Want::Statement => Ok(()),
                    Want::Location(_) => self.give(Operand::Loc(Loc::Arg(i))),
                    Want::Value { raw } => {
                        let v = self.arg(i, raw)?;
                        self.give_val(v)
                    }
                    Want::Data | Want::Element => err(ErrorKind::Grammar),
                };
            }
            _ => {}
        }
        if let Want::Location(kind) = want {
            return self.decode_location(kind, at);
        }
        // Data objects.
        let constant = match b {
            0x00 => Some((0, 1)),
            0x01 => Some((1, 1)),
            0xFF => Some((self.width.mask(), 1)),
            0x0A => Some((self.le(next, 1)?, 2)),
            0x0B => Some((self.le(next, 2)?, 3)),
            0x0C => Some((self.le(next, 4)?, 5)),
            0x0E => Some((self.le(next, 8)? & self.width.mask(), 9)),
            0x5B if self.byte(next).ok() == Some(0x30) => Some((INTERPRETER_REVISION, 2)),
            _ => None,
        };
        if let Some((v, len)) = constant {
            #[cfg(test)]
            self.note_ran(at);
            self.cur.pc = add(at, len)?;
            return match want {
                Want::Statement => Ok(()),
                _ => self.give_val(Val::Int(v)),
            };
        }
        if b == 0x0D {
            #[cfg(test)]
            crate::tests::ran(0x0D);
            return self.decode_string(want, next);
        }
        if b == 0x5B && self.byte(next).ok() == Some(0x31) {
            // `Debug` is write-only (§19.6.26): only a place.
            return err(ErrorKind::Grammar);
        }
        if name::starts_name(b) {
            return self.decode_name(want, at);
        }
        // An `Else` is only ever read after its `If` (`else_after`): met anywhere else it is out
        // of place, not reserved (the review of B1, F12).
        if b == ELSE {
            return err(ErrorKind::Grammar);
        }
        let info = op::decode(b, self.byte(next).ok()).ok_or(ErrorKind::Reserved {
            first: b,
            second: (b == 0x5B).then(|| self.byte(next).ok()).flatten(),
        })?;
        let data = matches!(info.op, Op::Buffer | Op::Package | Op::VarPackage);
        match want {
            Want::Statement => {}
            Want::Value { .. } if info.class == Class::Expression => {}
            Want::Data | Want::Element if data => {}
            _ => return err(ErrorKind::Grammar),
        }
        self.push_op(info, at)
    }

    fn push_op(&mut self, info: op::Info, at: u32) -> Result<()> {
        let mut pc = add(at, info.len)?;
        let mut end = 0;
        if info.pkg {
            let (len, after) = self.pkg_length(pc)?;
            end = add(pc, len)?;
            if end > self.enclosing_end() || after > end {
                return err(ErrorKind::PackageLength);
            }
            pc = after;
        }
        let arity = u8::try_from(info.args.len()).map_err(|_| ErrorKind::Grammar)?;
        let aux = if info.op == Op::While {
            u64::from(pc)
        } else {
            0
        };
        self.cur.pc = pc;
        self.push_frame(Frame::Op(OpFrame {
            op: info.op,
            at,
            end,
            kinds: info.args,
            arity,
            n: 0,
            args: [Operand::Empty; 7],
            phase: 0,
            aux,
            node: Namespace::root(),
        }))
    }

    fn decode_string(&mut self, want: Want, start: u32) -> Result<()> {
        let from = us(start);
        let limit = us(self.enclosing_end());
        // G13 (TASKS 64's ledger §3; step B5): every byte up to the NUL is a character, one above
        // 0x7F (§20.2.3 allows 0x01 to 0x7F) included, reported; a literal with no NUL before the
        // end of the range around it ends there, reported. The table's bytes are never written.
        let (len, terminated, high) = {
            let rest = self
                .aml()
                .get(from..limit.max(from))
                .ok_or(ErrorKind::Truncated)?;
            let (len, terminated) = match rest.iter().position(|&c| c == 0) {
                Some(n) => (n, true),
                None => (rest.len(), false),
            };
            (len, terminated, rest.iter().take(len).any(|&c| c > 0x7F))
        };
        // The scan read `len` bytes: work, counted a unit for each 64 and one more, whether or
        // not the string is kept.
        self.note_work(
            u64::try_from(len / 64)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        );
        let literal = At {
            table: self.cur.table,
            offset: start.saturating_sub(1),
        };
        #[cfg(test)]
        if high && crate::tests::planted(crate::tests::Fault::RefuseHighBytes) {
            return err(ErrorKind::Grammar);
        }
        if !terminated {
            self.tell(&crate::Report::Unterminated { at: literal });
        }
        if high {
            self.tell(&crate::Report::HighBytes { at: literal });
        }
        let end = add(start, u32::try_from(len).map_err(|_| ErrorKind::Truncated)?)?;
        self.cur.pc = if terminated { add(end, 1)? } else { end };
        if want == Want::Statement {
            return Ok(());
        }
        let mut bytes = mem::zeroed(len, &mut self.budget)?;
        if let Some(chars) = self
            .tables
            .get(us(self.cur.table))
            .and_then(|t| t.aml.get(from..from.saturating_add(len)))
        {
            bytes.copy_from_slice(chars);
        }
        let r = self.heap.alloc(Object::String(bytes), &mut self.budget)?;
        self.give_val(Val::Obj(r))
    }

    fn decode_name(&mut self, want: Want, at: u32) -> Result<()> {
        let (name, next) = name::parse(self.aml(), at)?;
        self.cur.pc = next;
        if want == Want::Element {
            // Load-time code's names are bound once every boot table has loaded, or when the
            // run-time load that made them ends (G1, step B5); a method's, when they are read.
            let bind = if self.in_load_time_code() {
                crate::object::Bind::Deferred
            } else {
                crate::object::Bind::Lazy
            };
            // A deferred name is listed until bound (B5-R2): room first, so that no element is
            // made deferred and left out of the list.
            if bind == crate::object::Bind::Deferred {
                mem::reserve(&mut self.deferred, 1, &mut self.budget)?;
            }
            let r = self.heap.alloc(
                Object::NameElem(crate::object::NameElem {
                    scope: self.scope(),
                    table: self.cur.table,
                    at,
                    bind,
                }),
                &mut self.budget,
            )?;
            if bind == crate::object::Bind::Deferred {
                // Room was made before the element: this push cannot fail.
                mem::push(&mut self.deferred, r, &mut self.budget)?;
            }
            return self.give_val(Val::Obj(r));
        }
        if want == Want::Data {
            return err(ErrorKind::Grammar);
        }
        // G1 (TASKS 64's ledger §3): a name that resolves to nothing is an error, which in
        // load-time code abandons its statement (`tolerate.rs`; step B5); one that resolves to a
        // method is a call taking the method's declared arguments.
        let node = self.resolve(name)?.ok_or(ErrorKind::NotFound)?;
        let obj = self.ns.obj(node)?;
        let argc = match self.heap.get(obj)? {
            Object::Method(m) => Some(m.argc()),
            Object::Osi => Some(1),
            _ => None,
        };
        if let Some(argc) = argc {
            return self.push_call(node, argc, at);
        }
        match want {
            Want::Statement => Ok(()),
            _ => {
                let v = self.read_node(node)?;
                self.give_val(v)
            }
        }
    }

    fn decode_location(&mut self, kind: Kind, at: u32) -> Result<()> {
        let b = self.byte(at)?;
        let next = add(at, 1)?;
        if b == 0x00 {
            if kind != Kind::Target {
                return err(ErrorKind::Grammar);
            }
            self.cur.pc = next;
            return self.give(Operand::Loc(Loc::Null));
        }
        if b == 0x5B && self.byte(next).ok() == Some(0x31) {
            if kind == Kind::Simple {
                return err(ErrorKind::Grammar);
            }
            #[cfg(test)]
            crate::tests::ran(0x5B31);
            self.cur.pc = add(at, 2)?;
            return self.give(Operand::Loc(Loc::Debug));
        }
        if name::starts_name(b) {
            let (name, after) = name::parse(self.aml(), at)?;
            self.cur.pc = after;
            // A name in a place is never called (E6: `UserTermObj` is undefined; the contract's
            // §4 takes no method invocation as a place).
            let loc = match self.resolve(name)? {
                Some(n) => Loc::Node(n),
                None if kind == Kind::SuperOpt => Loc::Missing,
                None => return err(ErrorKind::NotFound),
            };
            return self.give(Operand::Loc(loc));
        }
        if kind == Kind::Simple {
            return err(ErrorKind::Grammar);
        }
        match b {
            0x71 | 0x83 | 0x88 => {
                let info = op::decode(b, None).ok_or(ErrorKind::Grammar)?;
                self.push_op(info, at)
            }
            _ => err(ErrorKind::Grammar),
        }
    }

    // --- Packages ------------------------------------------------------------------------------

    /// A package's elements, one per step, after its count.
    fn package_step(&mut self, f: OpFrame) -> Result<()> {
        if f.phase == 0 {
            // The count is in: make the package (§19.6.102; uninitialized elements past the list).
            let count = match f.arg(0) {
                Operand::Raw(n) => n,
                Operand::Val(v) => self.int_of(v)?,
                _ => return err(ErrorKind::Grammar),
            };
            let count = usize::try_from(count).map_err(|_| ErrorKind::Limit(Limit::Memory))?;
            let elems = mem::filled(count, Val::None, &mut self.budget)?;
            let pkg = self.heap.alloc(Object::Package(elems), &mut self.budget)?;
            if let Some(Frame::Op(top)) = self.frames.last_mut() {
                top.phase = 1;
                top.aux = 0;
                if let Some(slot) = top.args.get_mut(6) {
                    *slot = Operand::Val(Val::Obj(pkg));
                }
            }
            return Ok(());
        }
        if self.cur.pc < f.end {
            return self.decode_element(f);
        }
        if self.cur.pc > f.end {
            return err(ErrorKind::PackageLength);
        }
        self.pop_frame();
        #[cfg(test)]
        self.note_ran(f.at);
        let Operand::Val(v) = f.arg(6) else {
            return err(ErrorKind::Grammar);
        };
        if let Val::Obj(r) = v
            && let Ok(Object::Package(elems)) = self.heap.get(r)
        {
            let count = u64::try_from(elems.len()).unwrap_or(u64::MAX);
            if f.aux > count {
                self.tell(&crate::Report::ExtraElements {
                    at: At {
                        table: self.cur.table,
                        offset: f.at,
                    },
                    count,
                    found: f.aux,
                });
            }
        }
        self.give_val(v)
    }

    /// Decodes one element: a data object, kept here, or a name, kept unresolved.
    fn decode_element(&mut self, f: OpFrame) -> Result<()> {
        // Decode it as if it were an operand of a one-operand frame, then move it in.
        let index = f.aux;
        let Operand::Val(Val::Obj(pkg)) = f.arg(6) else {
            return err(ErrorKind::Grammar);
        };
        let at = self.cur.pc;
        // Elements that are themselves frames (buffers, packages) are delivered by `give` to a
        // frame in phase 1, which `give` refuses: push a carrier frame that takes one `Data`
        // operand and puts it in the package when it completes.
        self.push_frame(Frame::Op(OpFrame {
            op: Op::Noop,
            at,
            end: 0,
            kinds: &[Kind::Data],
            arity: 1,
            n: 0,
            args: [
                Operand::Empty,
                Operand::Raw(index),
                Operand::Val(Val::Obj(pkg)),
                Operand::Empty,
                Operand::Empty,
                Operand::Empty,
                Operand::Empty,
            ],
            phase: 2,
            aux: 0,
            node: Namespace::root(),
        }))?;
        self.decode(Want::Element)
    }

    /// Puts a decoded element (a carrier frame's operand) into its package.
    pub(crate) fn place_element(&mut self, carrier: OpFrame) -> Result<()> {
        let (Operand::Val(v), Operand::Raw(index), Operand::Val(Val::Obj(pkg))) =
            (carrier.arg(0), carrier.arg(1), carrier.arg(2))
        else {
            return err(ErrorKind::Grammar);
        };
        let i = usize::try_from(index).map_err(|_| ErrorKind::Range)?;
        let dropped = match self.heap.get_mut(pkg)? {
            Object::Package(elems) => {
                // More elements than the count (a compile-time error by §19.6.102): the extra
                // ones are decoded, then dropped, and reported when the package ends (G14, TASKS
                // 64's ledger §3; step B5).
                #[cfg(test)]
                if i >= elems.len()
                    && crate::tests::planted(crate::tests::Fault::RefuseExtraElements)
                {
                    return err(ErrorKind::Range);
                }
                match elems.get_mut(i) {
                    Some(slot) => {
                        *slot = v;
                        false
                    }
                    None => true,
                }
            }
            _ => return err(ErrorKind::Grammar),
        };
        // A dropped element's names are never bound: nothing can read them (B5-R3).
        if dropped {
            self.never_bind(self.cur.table, carrier.at, self.cur.pc);
        }
        if let Some(Frame::Op(top)) = self.frames.last_mut() {
            top.aux = top.aux.saturating_add(1);
        }
        Ok(())
    }

    // --- Blocks and control flow -------------------------------------------------------------

    fn end_block(&mut self) -> Result<()> {
        let Some(Frame::Block {
            kind, end, pred, ..
        }) = self.pop_frame()
        else {
            return err(ErrorKind::Grammar);
        };
        match kind {
            BlockKind::Table | BlockKind::Scope | BlockKind::Else => Ok(()),
            BlockKind::Method => self.finish_call(Val::None),
            BlockKind::If => self.skip_else(),
            BlockKind::While => self.repeat_while(end, pred),
        }
    }

    /// After an `If` block or a false predicate: skips (`run` false) or enters (`run` true) the
    /// `Else` that may follow.
    fn skip_else(&mut self) -> Result<()> {
        self.else_after(false)
    }

    fn else_after(&mut self, run: bool) -> Result<()> {
        let pc = self.cur.pc;
        if pc >= self.enclosing_end() || self.byte(pc)? != ELSE {
            return Ok(());
        }
        #[cfg(test)]
        crate::tests::ran(u16::from(ELSE));
        let after_op = add(pc, 1)?;
        let (len, body) = self.pkg_length(after_op)?;
        let end = add(after_op, len)?;
        if end > self.enclosing_end() || body > end {
            return err(ErrorKind::PackageLength);
        }
        if run {
            self.cur.pc = body;
            let scope = self.scope();
            self.push_frame(Frame::Block {
                kind: BlockKind::Else,
                end,
                scope,
                pred: 0,
                stmt: self.cur,
            })
        } else {
            self.cur.pc = end;
            Ok(())
        }
    }

    /// A `While`'s body ended (or `Continue`): evaluate its predicate again.
    fn repeat_while(&mut self, end: u32, pred: u32) -> Result<()> {
        self.run.loops = self.run.loops.saturating_add(1);
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::NoLoopLimit) {
            self.run.loops = 0;
        }
        if self.run.loops > self.config.limits.loop_iterations {
            return err(ErrorKind::Limit(Limit::Loops));
        }
        self.check_time()?;
        self.cur.pc = pred;
        let args: &'static [Kind] = &[Kind::TermArg];
        self.push_frame(Frame::Op(OpFrame {
            op: Op::While,
            at: pred,
            end,
            kinds: args,
            arity: 1,
            n: 0,
            args: [Operand::Empty; 7],
            phase: 0,
            aux: u64::from(pred),
            node: Namespace::root(),
        }))
    }

    /// `If`'s predicate is in.
    pub(crate) fn branch_if(&mut self, f: &OpFrame, taken: bool) -> Result<()> {
        if taken {
            let scope = self.scope();
            self.push_frame(Frame::Block {
                kind: BlockKind::If,
                end: f.end,
                scope,
                pred: 0,
                stmt: self.cur,
            })
        } else {
            self.cur.pc = f.end;
            self.else_after(true)
        }
    }

    /// `While`'s predicate is in.
    pub(crate) fn branch_while(&mut self, f: &OpFrame, taken: bool) -> Result<()> {
        if taken {
            let scope = self.scope();
            let pred = u32::try_from(f.aux).map_err(|_| ErrorKind::Grammar)?;
            self.push_frame(Frame::Block {
                kind: BlockKind::While,
                end: f.end,
                scope,
                pred,
                stmt: self.cur,
            })
        } else {
            self.cur.pc = f.end;
            Ok(())
        }
    }

    /// `Break` (`again` false) or `Continue` (`again` true): out to the innermost `While` of
    /// this method.
    pub(crate) fn leave_loop(&mut self, again: bool) -> Result<()> {
        loop {
            match self.frames.last().copied() {
                Some(Frame::Block {
                    kind: BlockKind::While,
                    end,
                    pred,
                    ..
                }) => {
                    self.pop_frame();
                    if again {
                        return self.repeat_while(end, pred);
                    }
                    self.cur.pc = end;
                    return Ok(());
                }
                Some(Frame::Block {
                    kind: BlockKind::If | BlockKind::Else,
                    ..
                }) if self.frames.len() > self.base => {
                    self.pop_frame();
                }
                _ => return err(ErrorKind::NotInLoop),
            }
        }
    }

    /// A pushed scope-opening named object (`Scope`, `Device` …): its body runs in `scope`.
    pub(crate) fn open_scope(&mut self, f: &OpFrame, scope: NodeId) -> Result<()> {
        self.push_frame(Frame::Block {
            kind: BlockKind::Scope,
            end: f.end,
            scope,
            pred: 0,
            stmt: self.cur,
        })
    }

    // --- Calls -------------------------------------------------------------------------------

    fn push_call(&mut self, node: NodeId, argc: u8, at: u32) -> Result<()> {
        self.push_frame(Frame::Op(OpFrame {
            op: Op::Call,
            at,
            end: 0,
            kinds: CALL_KINDS,
            arity: argc.min(7),
            n: 0,
            args: [Operand::Empty; 7],
            phase: 0,
            aux: 0,
            node,
        }))
    }

    /// Enters the method `node` with `args`.
    pub(crate) fn begin_call(&mut self, node: NodeId, args: [Val; 7]) -> Result<()> {
        let obj = self.ns.obj(node)?;
        let m = match self.heap.get(obj)? {
            Object::Method(m) => *m,
            Object::Osi => {
                let v = self.osi(args.first().copied().unwrap_or(Val::None))?;
                return self.give_val(v);
            }
            _ => return err(ErrorKind::Type),
        };
        if self.tables.get(us(m.table)).is_none_or(|t| t.failed) {
            return err(ErrorKind::TableFailed);
        }
        self.run.depth = self.run.depth.saturating_add(1);
        if self.run.depth > self.config.limits.call_depth {
            return err(ErrorKind::Limit(Limit::Depth));
        }
        self.check_time()?;
        self.serial = self.serial.wrapping_add(1);
        let serialized = m.flags & 8 != 0;
        let call = CallFrame {
            serial: self.serial,
            method: node,
            ret: self.cur,
            locals: [Val::None; 8],
            args,
            serialized,
        };
        // A serialized method holds its implicit mutex at its sync level while it runs
        // (§19.6.85). With one evaluation at a time nothing else can hold it; the sync level is
        // checked as for `Acquire`.
        if serialized {
            self.hold_implicit(self.serial, m.flags >> 4)?;
        }
        self.push_frame(Frame::Call)?;
        mem::push(&mut self.calls, call, &mut self.budget)?;
        self.trace_enter(node);
        self.push_owner(Owner::Call(self.serial))?;
        self.cur = Cursor {
            table: m.table,
            pc: m.start,
        };
        self.push_frame(Frame::Block {
            kind: BlockKind::Method,
            end: m.end,
            scope: m.node,
            pred: 0,
            stmt: self.cur,
        })
    }

    /// Returns from the innermost method with `v` (a copy is made by `Return`).
    pub(crate) fn finish_call(&mut self, v: Val) -> Result<()> {
        loop {
            match self.pop_frame() {
                Some(Frame::Call) => break,
                Some(_) => {}
                None => return err(ErrorKind::Grammar),
            }
        }
        let call = self.calls.pop().ok_or(ErrorKind::Grammar)?;
        self.trace(&Trace::Leave);
        if call.serialized {
            self.drop_implicit(call.serial);
        }
        // A mutex the method took and did not release stays held (G2, step B5).
        #[cfg(test)]
        if crate::tests::planted(crate::tests::Fault::ReleaseOnReturn) {
            self.release_call_holds(call.serial);
        }
        self.pop_owner(true);
        self.run.depth = self.run.depth.saturating_sub(1);
        self.cur = call.ret;
        if v == Val::None {
            // G6, as ACPICA's default (TASKS 64's ledger §3; not its lenient mode): no value where
            // one is needed is an error; as a statement it is nothing.
            if self.top_wants_value() {
                return err(ErrorKind::NoValue);
            }
            return Ok(());
        }
        self.give_val(v)
    }

    pub(crate) fn call_frame(&self, serial: u64) -> Result<&CallFrame> {
        self.calls
            .iter()
            .rev()
            .find(|c| c.serial == serial)
            .ok_or_else(|| Error::from(ErrorKind::Stale))
    }

    pub(crate) fn call_frame_mut(&mut self, serial: u64) -> Result<&mut CallFrame> {
        self.calls
            .iter_mut()
            .rev()
            .find(|c| c.serial == serial)
            .ok_or_else(|| Error::from(ErrorKind::Stale))
    }

    pub(crate) fn current_call(&self) -> Result<&CallFrame> {
        self.calls
            .last()
            .ok_or_else(|| Error::from(ErrorKind::Grammar))
    }

    /// Local `i` of the running method.
    pub(crate) fn local(&self, i: u8) -> Result<Val> {
        let c = self.current_call()?;
        c.locals
            .get(usize::from(i))
            .copied()
            .ok_or_else(|| Error::from(ErrorKind::Grammar))
    }

    /// Argument `i` of the running method; a reference in it is followed unless `raw`
    /// (Table 19.9).
    pub(crate) fn arg(&mut self, i: u8, raw: bool) -> Result<Val> {
        let v = {
            let c = self.current_call()?;
            c.args
                .get(usize::from(i))
                .copied()
                .ok_or(ErrorKind::Grammar)?
        };
        match v {
            Val::Ref(t) if !raw => self.read_ref(t),
            v => Ok(v),
        }
    }

    // --- Reading objects ---------------------------------------------------------------------

    /// The value a named object gives as an operand: an integer by value, a field read, anything
    /// else by handle.
    pub(crate) fn read_node(&mut self, node: NodeId) -> Result<Val> {
        let obj = self.ns.obj(node)?;
        let v = match self.heap.get(obj)? {
            Object::Integer(v) => Val::Int(*v & self.width.mask()),
            Object::Reference(t) => Val::Ref(*t),
            Object::Uninitialized => Val::None,
            Object::BufferField(f) => {
                let f = *f;
                return self.read_buffer_field(&f);
            }
            Object::FieldUnit(_) => return self.read_field(node),
            Object::Alias(n) => {
                let n = *n;
                let heap = &self.heap;
                let n = self.ns.unalias(n, |o| Self::alias_target(heap, o))?;
                let o = self.ns.obj(n)?;
                if matches!(self.heap.get(o)?, Object::Alias(_)) {
                    return err(ErrorKind::NotFound);
                }
                return self.read_node(n);
            }
            _ => Val::Obj(obj),
        };
        Ok(v)
    }

    /// What a reference points at, read as an operand.
    pub(crate) fn read_ref(&mut self, t: RefTarget) -> Result<Val> {
        match t {
            RefTarget::Node(n) => self.read_node(n),
            RefTarget::Local { call, slot } => self
                .call_frame(call)?
                .locals
                .get(usize::from(slot))
                .copied()
                .ok_or_else(|| Error::from(ErrorKind::Grammar)),
            RefTarget::Arg { call, slot } => self
                .call_frame(call)?
                .args
                .get(usize::from(slot))
                .copied()
                .ok_or_else(|| Error::from(ErrorKind::Grammar)),
            RefTarget::Elem { pkg, index } => {
                let e = match self.heap.get(pkg)? {
                    Object::Package(v) => v
                        .get(usize::try_from(index).map_err(|_| ErrorKind::Range)?)
                        .copied()
                        .ok_or(ErrorKind::Range)?,
                    _ => return err(ErrorKind::Type),
                };
                match e {
                    // Dereferencing an uninitialized element is fatal (§19.6.30).
                    Val::None => err(ErrorKind::Uninitialized),
                    v => self.element(v),
                }
            }
            RefTarget::Byte { obj, index } => {
                let i = usize::try_from(index).map_err(|_| ErrorKind::Range)?;
                match self.heap.get(obj)? {
                    Object::Buffer(b) | Object::String(b) => {
                        Ok(Val::Int(u64::from(*b.get(i).ok_or(ErrorKind::Range)?)))
                    }
                    _ => err(ErrorKind::Type),
                }
            }
        }
    }

    /// A package element as an operand: a name kept in it is resolved now (§19.6.102): to the
    /// object's data, or to a reference for an object that is not data.
    pub(crate) fn element(&mut self, v: Val) -> Result<Val> {
        let Val::Obj(r) = v else {
            return Ok(v);
        };
        let Object::NameElem(e) = *self.heap.get(r)? else {
            return Ok(v);
        };
        // A name bound to nothing is an empty element (G1, step B5).
        if e.bind == crate::object::Bind::Empty {
            return Ok(Val::None);
        }
        let node = self.resolve_elem(e)?;
        let obj = self.ns.obj(node)?;
        if self.heap.get(obj)?.is_data() {
            self.read_node(node)
        } else {
            Ok(Val::Ref(RefTarget::Node(node)))
        }
    }

    /// Resolves a name kept in a package.
    pub(crate) fn resolve_elem(&self, e: crate::object::NameElem) -> Result<NodeId> {
        match e.bind {
            crate::object::Bind::Node(n) if self.ns.exists(n) => return Ok(n),
            crate::object::Bind::Node(_) => return err(ErrorKind::Stale),
            crate::object::Bind::Empty => return err(ErrorKind::NotFound),
            crate::object::Bind::Lazy | crate::object::Bind::Deferred => {}
        }
        let aml = self
            .tables
            .get(us(e.table))
            .map_or(&[][..], |t| t.aml.as_slice());
        let (name, _) = name::parse(aml, e.at)?;
        let heap = &self.heap;
        let scope = if self.ns.exists(e.scope) {
            e.scope
        } else {
            return err(ErrorKind::Stale);
        };
        let n = self
            .ns
            .lookup(scope, &InTable { name, aml }, |o| {
                Self::alias_target(heap, o)
            })?
            .ok_or(ErrorKind::NotFound)?;
        self.ns.unalias(n, |o| Self::alias_target(heap, o))
    }

    /// Notes the opcode at `at` as run, for the coverage test.
    #[cfg(test)]
    pub(crate) fn note_ran(&self, at: u32) {
        let b = self.byte(at).unwrap_or(0);
        let second = self.byte(at.saturating_add(1)).unwrap_or(0);
        let code = match (b, second) {
            (0x5B, s) => 0x5B00 | u16::from(s),
            (0x92, s @ 0x93..=0x95) => 0x9200 | u16::from(s),
            (b, _) => u16::from(b),
        };
        crate::tests::ran(code);
    }

    // --- The collector -----------------------------------------------------------------------

    /// Collects every object unreachable from the namespace and the frames.
    pub(crate) fn collect_garbage(&mut self) {
        self.heap.clear_marks();
        let mut ok = true;
        for r in self.ns.objects() {
            ok &= self.heap.mark(Val::Obj(r), &mut self.budget);
        }
        for f in &self.frames {
            match f {
                Frame::Op(f) => {
                    for a in f.args {
                        ok &= match a {
                            Operand::Val(v) => self.heap.mark(v, &mut self.budget),
                            Operand::Loc(Loc::Ref(t)) => {
                                self.heap.mark(Val::Ref(t), &mut self.budget)
                            }
                            _ => true,
                        };
                    }
                }
                Frame::Resolve(r) => {
                    if let Some(v) = r.got {
                        ok &= self.heap.mark(v, &mut self.budget);
                    }
                }
                // The operator a load completes keeps its operands: `LoadTable`'s parameter,
                // until the step that stores it.
                Frame::Loading(crate::load::Loading { args, .. })
                | Frame::Parameter(crate::load::Parameter { args, .. }) => {
                    for a in *args {
                        ok &= match a {
                            Operand::Val(v) => self.heap.mark(v, &mut self.budget),
                            Operand::Loc(Loc::Ref(t)) => {
                                self.heap.mark(Val::Ref(t), &mut self.budget)
                            }
                            _ => true,
                        };
                    }
                }
                _ => {}
            }
        }
        for c in &self.calls {
            for v in c.locals.into_iter().chain(c.args) {
                ok &= self.heap.mark(v, &mut self.budget);
            }
        }
        ok &= self.heap.mark(self.result, &mut self.budget);
        for h in &self.held {
            if let Some(r) = h.mutex {
                ok &= self.heap.mark(Val::Obj(r), &mut self.budget);
            }
        }
        for &r in &self.deferred {
            ok &= self.heap.mark(Val::Obj(r), &mut self.budget);
        }
        if ok && self.heap.trace(&mut self.budget) {
            self.heap.sweep(&mut self.budget);
        } else {
            self.heap.clear_marks();
        }
    }
}
