//! Values across the API, and the API that takes and gives them: lookups, evaluations and the
//! walk of the namespace.
//!
//! A value handed out is a copy, detached from the interpreter. Packages are copied with a work
//! list, never by recursion, and no deeper than 64 levels (a deeper one is refused with
//! [`Limit::Nesting`]), so dropping a value never recurses deeply either.

use alloc::vec::Vec;

use crate::Interpreter;
use crate::error::{Error, ErrorKind, Limit, Result, err};
use crate::host::Host;
use crate::mem::{self, Budget};
use crate::name::{Seg, TextName};
use crate::namespace::{Namespace, Owner};
use crate::object::{NodeId, ObjRef, Object, RefTarget, Val};

/// The deepest package nesting a value crossing the API may have.
pub(crate) const NESTING: usize = 64;

/// A node of the namespace, as the API names it. It stays valid until the node is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node(pub(crate) NodeId);

/// A value given to or returned by the interpreter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// No value: an uninitialized package element.
    Uninitialized,
    /// An integer (at most the DSDT's width).
    Integer(u64),
    /// A string's characters, without the NUL.
    String(Vec<u8>),
    /// A buffer.
    Buffer(Vec<u8>),
    /// A package.
    Package(Vec<Value>),
    /// A reference to a named object that is not data: a device, a method, a mutex …
    Reference(Node),
}

/// A node's value when reading it runs nothing and asks the host nothing ([`Interpreter::plain`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plain<'a> {
    /// An integer.
    Integer(u64),
    /// A string's characters, without the NUL.
    String(&'a [u8]),
}

/// One level of a package being copied out.
struct Out {
    src: ObjRef,
    next: usize,
    items: Vec<Value>,
}

/// One level of a package being copied in.
struct In<'a> {
    src: &'a [Value],
    next: usize,
    dst: ObjRef,
}

impl<H: Host> Interpreter<H> {
    // --- The namespace -----------------------------------------------------------------------

    /// The node a path names: absolute (`\_SB.PCI0`) or from the root.
    ///
    /// # Errors
    /// [`ErrorKind::Path`] for a malformed path.
    pub fn lookup(&self, path: &str) -> Result<Option<Node>> {
        let p = TextName::parse(path)?;
        let heap = &self.heap;
        let found = self
            .ns
            .lookup(Namespace::root(), &p, |o| Self::alias_target(heap, o))?;
        Ok(found.map(Node))
    }

    /// The type code of a node's object (ACPI 6.6 Table 19.36).
    ///
    /// # Errors
    /// [`ErrorKind::Stale`] for a node that has been removed.
    pub fn object_type(&self, node: Node) -> Result<u64> {
        Ok(self.heap.get(self.ns.obj(node.0)?)?.type_code())
    }

    /// What `node` holds, if it is plain data: an integer or a string, after following aliases
    /// as name lookups do. `None` for every other object, among them each whose reading could run
    /// AML or reach the host: a method, a field unit, a buffer field, a package whose elements
    /// name such things. Runs nothing, asks the host nothing and counts nothing (the review of B3,
    /// B3-1: the kernel's boot proof reads a device's `_HID` with it).
    ///
    /// # Errors
    /// [`ErrorKind::Stale`] for a node that has been removed; [`ErrorKind::NotFound`] for an
    /// alias chain longer than lookups follow.
    pub fn plain(&self, node: Node) -> Result<Option<Plain<'_>>> {
        let heap = &self.heap;
        let target = self.ns.unalias(node.0, |o| Self::alias_target(heap, o))?;
        Ok(match self.heap.get(self.ns.obj(target)?)? {
            Object::Integer(i) => Some(Plain::Integer(*i)),
            Object::String(s) => Some(Plain::String(s.as_slice())),
            _ => None,
        })
    }

    /// A node's name segment.
    ///
    /// # Errors
    /// [`ErrorKind::Stale`] for a node that has been removed.
    pub fn name_of(&self, node: Node) -> Result<[u8; 4]> {
        self.ns.name(node.0)
    }

    /// A node's parent (`None` for the root).
    pub fn parent(&self, node: Node) -> Option<Node> {
        self.ns.parent(node.0).map(Node)
    }

    /// A node's first child, in definition order.
    pub fn first_child(&self, node: Node) -> Option<Node> {
        self.ns.first_child(node.0).map(Node)
    }

    /// A node's next sibling, in definition order.
    pub fn next_sibling(&self, node: Node) -> Option<Node> {
        self.ns.next_sibling(node.0).map(Node)
    }

    /// The root.
    pub const fn root(&self) -> Node {
        Node(Namespace::root())
    }

    /// Whether the node was made by a definition block (rather than predefined).
    ///
    /// # Errors
    /// [`ErrorKind::Stale`] for a node that has been removed.
    pub fn is_loaded(&self, node: Node) -> Result<bool> {
        Ok(matches!(self.ns.owner(node.0)?, Owner::Table(_)))
    }

    /// A node's path from the root, segment by segment. The vector is the caller's; its memory is
    /// not counted against the limit, but is refused if the limit's room could not hold it.
    ///
    /// # Errors
    /// [`ErrorKind::Stale`] for a node that has been removed.
    pub fn path_of(&self, node: Node) -> Result<Vec<Seg>> {
        let mut room = Budget::new(self.budget.limit().saturating_sub(self.budget.used()));
        let mut out = Vec::new();
        self.ns.path(node.0, &mut out, &mut room)?;
        Ok(out)
    }

    // --- Evaluation --------------------------------------------------------------------------

    /// Evaluates the object a path names: runs a method with `args`, or reads a data object.
    ///
    /// # Errors
    /// [`ErrorKind::NotFound`] if nothing is there; whatever the method does wrong.
    pub fn evaluate(&mut self, path: &str, args: &[Value]) -> Result<Option<Value>> {
        let node = self.lookup(path)?.ok_or(Error::new(ErrorKind::NotFound))?;
        self.evaluate_node(node, args)
    }

    /// Evaluates a node: see [`Interpreter::evaluate`].
    ///
    /// # Errors
    /// As [`Interpreter::evaluate`].
    pub fn evaluate_node(&mut self, node: Node, args: &[Value]) -> Result<Option<Value>> {
        let marks = self.begin_evaluation()?;
        let outcome = self.run_node(node.0, args, marks.frames);
        let out = match outcome {
            // Every mutex is released before an evaluation ends (§19.6.89); one still held is
            // released now and reported (G2, strict until the reading session answers).
            Ok(_) if self.release_held(marks.held) => Err(Error::new(ErrorKind::MutexHeld)),
            Ok(v) => {
                // `export` may run methods (a PCI_Config region's `_ADR`, through
                // `with_address`): what they take is inside the evaluation's marks, released and
                // reported the same way, and a failed export is undone like any failure (the
                // review of B2, B2-6).
                let r = self.export(v);
                let kept = self.release_held(marks.held);
                match r {
                    Err(e) => {
                        self.abort(marks);
                        Err(e)
                    }
                    Ok(_) if kept => Err(Error::new(ErrorKind::MutexHeld)),
                    ok => ok,
                }
            }
            Err(e) => {
                self.abort(marks);
                Err(e)
            }
        };
        self.end_evaluation();
        if let Err(e) = &out {
            self.trace_limit(e);
        }
        out
    }

    fn run_node(&mut self, node: NodeId, args: &[Value], base: usize) -> Result<Val> {
        let heap = &self.heap;
        let node = self.ns.unalias(node, |o| Self::alias_target(heap, o))?;
        let obj = self.ns.obj(node)?;
        let argc = match self.heap.get(obj)? {
            Object::Method(m) => Some(usize::from(m.argc())),
            Object::Osi => Some(1),
            _ => None,
        };
        let Some(argc) = argc else {
            if !args.is_empty() {
                return err(ErrorKind::Type);
            }
            return self.with_address(|s| s.read_node(node));
        };
        // The number of arguments is fixed by the method's declaration (§5.5.2.1).
        if args.len() != argc {
            return err(ErrorKind::Type);
        }
        let mut vals = [Val::None; 7];
        for (slot, a) in vals.iter_mut().zip(args) {
            *slot = self.import(a)?;
            // Keep each imported argument reachable while the next is made: the collector does
            // not run until the call's frames hold them, so nothing more is needed.
        }
        self.begin_call(node, vals)?;
        self.run_frames(base)?;
        Ok(self.result)
    }

    /// A value from the API, into the heap. Packages are built level by level.
    fn import(&mut self, v: &Value) -> Result<Val> {
        let Value::Package(items) = v else {
            return self.import_flat(v);
        };
        let top = self.new_package(items.len())?;
        let mut stack: Vec<In<'_>> = Vec::new();
        let r = (|| {
            mem::push(
                &mut stack,
                In {
                    src: items,
                    next: 0,
                    dst: top,
                },
                &mut self.budget,
            )?;
            while let Some(level) = stack.last_mut() {
                let Some(item) = level.src.get(level.next) else {
                    stack.pop();
                    continue;
                };
                let index = level.next;
                let dst = level.dst;
                level.next = level.next.saturating_add(1);
                let val = if let Value::Package(inner) = item {
                    if stack.len() >= NESTING {
                        return err(ErrorKind::Limit(Limit::Nesting));
                    }
                    let p = self.new_package(inner.len())?;
                    mem::push(
                        &mut stack,
                        In {
                            src: inner,
                            next: 0,
                            dst: p,
                        },
                        &mut self.budget,
                    )?;
                    Val::Obj(p)
                } else {
                    self.import_flat(item)?
                };
                if let Object::Package(e) = self.heap.get_mut(dst)?
                    && let Some(slot) = e.get_mut(index)
                {
                    *slot = val;
                }
            }
            Ok(())
        })();
        mem::release(stack, &mut self.budget);
        r.map(|()| Val::Obj(top))
    }

    fn new_package(&mut self, n: usize) -> Result<ObjRef> {
        let elems = mem::filled(n, Val::None, &mut self.budget)?;
        self.heap.alloc(Object::Package(elems), &mut self.budget)
    }

    fn import_flat(&mut self, v: &Value) -> Result<Val> {
        Ok(match v {
            Value::Uninitialized => Val::None,
            Value::Integer(i) => Val::Int(*i & self.width.mask()),
            Value::String(s) => {
                let b = mem::copy_bytes(s, &mut self.budget)?;
                self.new_string(b)?
            }
            Value::Buffer(s) => {
                let b = mem::copy_bytes(s, &mut self.budget)?;
                self.new_buffer(b)?
            }
            Value::Reference(n) => {
                self.ns.obj(n.0)?;
                Val::Ref(RefTarget::Node(n.0))
            }
            Value::Package(_) => return err(ErrorKind::Type),
        })
    }

    /// A value out of the heap, for the API. `None` if there is none.
    pub(crate) fn export(&mut self, v: Val) -> Result<Option<Value>> {
        let v = match v {
            Val::None => return Ok(None),
            other => self.with_address(|s| s.settle(other))?,
        };
        let mut room = Budget::new(self.budget.limit().saturating_sub(self.budget.used()));
        let Val::Obj(top) = v else {
            return self.export_flat(v, &mut room).map(Some);
        };
        if !matches!(self.heap.get(top)?, Object::Package(_)) {
            return self.export_flat(v, &mut room).map(Some);
        }
        let mut stack: Vec<Out> = Vec::new();
        mem::push(
            &mut stack,
            Out {
                src: top,
                next: 0,
                items: Vec::new(),
            },
            &mut room,
        )?;
        loop {
            let Some(level) = stack.last_mut() else {
                return err(ErrorKind::Grammar);
            };
            let src = level.src;
            let k = level.next;
            let e = match self.heap.get(src)? {
                Object::Package(e) => e.get(k).copied(),
                _ => None,
            };
            let Some(e) = e else {
                // This level is done: hand it to the one below, or return it.
                let done = stack.pop().ok_or(ErrorKind::Grammar)?;
                let value = Value::Package(done.items);
                match stack.last_mut() {
                    Some(parent) => mem::push(&mut parent.items, value, &mut room)?,
                    None => return Ok(Some(value)),
                }
                continue;
            };
            level.next = k.saturating_add(1);
            // Each element exported is work under the evaluation's limits, its name's lookup and
            // its bytes included (the review of B1, F1).
            self.charge(1)?;
            let e = match e {
                Val::None => Val::None,
                other => self.with_address(|s| s.settle(other))?,
            };
            self.charge_pending()?;
            let nested = match e {
                Val::Obj(r) => matches!(self.heap.get(r)?, Object::Package(_)).then_some(r),
                _ => None,
            };
            if let Some(r) = nested {
                if stack.len() >= NESTING {
                    return err(ErrorKind::Limit(Limit::Nesting));
                }
                mem::push(
                    &mut stack,
                    Out {
                        src: r,
                        next: 0,
                        items: Vec::new(),
                    },
                    &mut room,
                )?;
            } else {
                let value = match e {
                    Val::None => Value::Uninitialized,
                    other => self.export_flat(other, &mut room)?,
                };
                if let Some(level) = stack.last_mut() {
                    mem::push(&mut level.items, value, &mut room)?;
                }
            }
        }
    }

    /// A value made ready to leave: names in packages resolved, references to data followed.
    fn settle(&mut self, v: Val) -> Result<Val> {
        let mut v = v;
        for _ in 0..16 {
            v = match v {
                Val::Obj(r) if matches!(self.heap.get(r)?, Object::NameElem(_)) => {
                    self.element(v)?
                }
                Val::Ref(RefTarget::Node(n)) => {
                    let obj = self.ns.obj(n)?;
                    if self.heap.get(obj)?.is_data() {
                        self.read_node(n)?
                    } else {
                        return Ok(v);
                    }
                }
                Val::Ref(t) => self.read_ref(t)?,
                other => return Ok(other),
            };
        }
        err(ErrorKind::Type)
    }

    fn export_flat(&mut self, v: Val, room: &mut Budget) -> Result<Value> {
        Ok(match v {
            Val::None => Value::Uninitialized,
            Val::Int(i) => Value::Integer(i),
            Val::Ref(RefTarget::Node(n)) => Value::Reference(crate::Node(n)),
            Val::Ref(_) => return err(ErrorKind::Type),
            Val::Obj(r) => match self.heap.get(r)? {
                Object::Integer(i) => Value::Integer(*i),
                Object::String(s) | Object::Buffer(s) => {
                    let len = u64::try_from(s.len()).unwrap_or(u64::MAX);
                    self.charge(len / 64)?;
                    match self.heap.get(r)? {
                        Object::String(s) => Value::String(mem::copy_bytes(s, room)?),
                        Object::Buffer(b) => Value::Buffer(mem::copy_bytes(b, room)?),
                        _ => return err(ErrorKind::Stale),
                    }
                }
                Object::Scope { node, .. } => Value::Reference(crate::Node(*node)),
                Object::Method(m) => Value::Reference(crate::Node(m.node)),
                _ => return err(ErrorKind::Type),
            },
        })
    }
}
