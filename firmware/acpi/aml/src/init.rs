//! Namespace initialization (ACPI 6.6 §6.3.7, §6.5.1 and Table 6.67) and `_REG` (§6.5.4).
//!
//! - [`Interpreter::initialize`] binds the names load-time code put in packages (step B5), then
//!   runs `\_SB._INI` if there is one, then walks the devices and
//!   processors depth first in definition order: each one's `_STA` (none means present and
//!   functioning), then its `_INI` if it is present, then its children if it is present or
//!   functioning. A failing `_STA` or `_INI` is reported, and the walk goes on with the next
//!   sibling, skipping what is below the device.
//! - [`Interpreter::connect_space`] runs `_REG (space, 1)` (or `0` on a disconnect) once in each
//!   scope that declares a region of that space, scopes in definition order. SystemMemory and SystemIO are always available and get
//!   none, nor do PCI_Config regions declared in a scope with a `_BBN` (a root bus), as §6.5.4
//!   says.
//!
//! Each `_STA`, `_INI` and `_REG` is an evaluation of its own, with its own limits, and **the walk
//! as a whole is bounded too** (the review of B2, B2-7): by the limits' time, read from the walk's
//! start, and their steps, summed over every evaluation it runs and every node it visits. Each
//! evaluation is given the less of its own limits and what the walk has left; when the walk has
//! nothing left it stops and returns the limit it reached. The walk itself allocates nothing but
//! what those evaluations do, and recurses nowhere: it follows the namespace's parent and sibling
//! links.

use alloc::vec::Vec;

use crate::Interpreter;
use crate::error::{Error, ErrorKind, Limit, Result};
use crate::host::{Host, Space};
use crate::mem;
use crate::object::{NodeId, Object, ScopeKind};
use crate::value::{Node, Value};

/// What the initialization walk did with one device or processor (or with `\_SB` for its `_INI`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitOutcome {
    /// Present: `_INI` ran (`ini` true) or there was none; its children were examined.
    Initialized { status: u64, ini: bool },
    /// Not present but functioning: no `_INI`, its children examined.
    ChildrenOnly { status: u64 },
    /// Neither present nor functioning: skipped with its children.
    Absent { status: u64 },
    /// `_STA` failed: skipped with its children.
    StaFailed(Error),
    /// `_INI` failed: its children skipped.
    IniFailed { status: u64, error: Error },
}

/// A walk's count against its bound.
struct Walk {
    started: u64,
    steps: u64,
}

/// `_STA`'s bits: present, functioning.
const PRESENT: u64 = 1;
const FUNCTIONING: u64 = 8;

impl<H: Host> Interpreter<H> {
    fn is_device(&self, n: NodeId) -> bool {
        self.ns
            .obj(n)
            .ok()
            .and_then(|o| self.heap.get(o).ok())
            .is_some_and(|o| {
                matches!(
                    o,
                    Object::Scope {
                        kind: ScopeKind::Device | ScopeKind::Processor { .. },
                        ..
                    }
                )
            })
    }

    fn start_walk(&mut self) -> Walk {
        Walk {
            started: self.host.now_100ns(),
            steps: 0,
        }
    }

    /// What the walk has left of the limits' time and steps; the limit it reached if nothing.
    fn walk_left(&mut self, w: &mut Walk) -> Result<(u64, u64)> {
        w.steps = w.steps.saturating_add(self.ns.take_work());
        let limits = self.config.limits;
        let used = self.host.now_100ns().saturating_sub(w.started);
        let time = limits
            .time_100ns
            .checked_sub(used)
            .filter(|t| *t > 0)
            .ok_or(ErrorKind::Limit(Limit::Time))?;
        let steps = limits
            .steps
            .checked_sub(w.steps)
            .filter(|s| *s > 0)
            .ok_or(ErrorKind::Limit(Limit::Steps))?;
        Ok((time, steps))
    }

    /// A walk stopped by its bound: told to a tracing host, and returned.
    fn walk_cut(&mut self, e: Error) -> Error {
        self.trace_limit(&e);
        e
    }

    /// Runs the method or reads the object `seg` under `n`, if there is one, within what the walk
    /// has left.
    fn run_child(
        &mut self,
        w: &mut Walk,
        n: NodeId,
        seg: [u8; 4],
        args: &[Value],
    ) -> Result<Option<Result<Option<Value>>>> {
        let Some(c) = self.ns.child(n, seg) else {
            return Ok(None);
        };
        let (time, steps) = self.walk_left(w)?;
        let own = self.config.limits;
        self.config.limits.time_100ns = own.time_100ns.min(time);
        self.config.limits.steps = own.steps.min(steps);
        let r = self.evaluate_node(Node(c), args);
        self.config.limits = own;
        w.steps = w.steps.saturating_add(self.run.steps);
        Ok(Some(r))
    }

    /// One device: its outcome, and whether to walk into its children.
    fn init_device(&mut self, w: &mut Walk, n: NodeId) -> Result<(InitOutcome, bool)> {
        let status = match self.run_child(w, n, *b"_STA", &[])? {
            None => 0x0F,
            Some(Ok(Some(Value::Integer(s)))) => s & 0xFFFF_FFFF,
            Some(Ok(_)) => {
                return Ok((InitOutcome::StaFailed(Error::new(ErrorKind::Type)), false));
            }
            Some(Err(e)) => return Ok((InitOutcome::StaFailed(e), false)),
        };
        let present = status & PRESENT != 0;
        let functioning = status & FUNCTIONING != 0;
        if !present {
            return Ok(if functioning {
                (InitOutcome::ChildrenOnly { status }, true)
            } else {
                (InitOutcome::Absent { status }, false)
            });
        }
        Ok(match self.run_child(w, n, *b"_INI", &[])? {
            None => (InitOutcome::Initialized { status, ini: false }, true),
            Some(Ok(_)) => (InitOutcome::Initialized { status, ini: true }, true),
            Some(Err(error)) => (InitOutcome::IniFailed { status, error }, false),
        })
    }

    /// The initialization walk (Table 6.67). `report` hears every device and processor it
    /// reaches, and `\_SB` if it has an `_INI`.
    ///
    /// # Errors
    /// [`ErrorKind::Limit`] (time or steps) if the walk as a whole reached the limits; it stops
    /// there, and what it reached before was reported.
    pub fn initialize(&mut self, report: &mut dyn FnMut(Node, InitOutcome)) -> Result<()> {
        // Every boot table has loaded: the names load-time code put in packages are bound first,
        // if the host has not bound them (G1, step B5). Only names not yet bound are.
        self.bind_deferred()?;
        let mut w = self.start_walk();
        self.initialize_within(&mut w, report)
            .map_err(|e| self.walk_cut(e))
    }

    fn initialize_within(
        &mut self,
        w: &mut Walk,
        report: &mut dyn FnMut(Node, InitOutcome),
    ) -> Result<()> {
        let root = crate::namespace::Namespace::root();
        if let Some(sb) = self.ns.child(root, *b"_SB_")
            && let Some(r) = self.run_child(w, sb, *b"_INI", &[])?
        {
            let outcome = match r {
                Ok(_) => InitOutcome::Initialized {
                    status: 0x0F,
                    ini: true,
                },
                Err(error) => InitOutcome::IniFailed {
                    status: 0x0F,
                    error,
                },
            };
            report(Node(sb), outcome);
        }
        let mut next = self.ns.first_child(root);
        while let Some(cur) = next {
            // Each node visited is a step of the walk.
            w.steps = w.steps.saturating_add(1);
            let descend = if self.is_device(cur) {
                let (outcome, descend) = self.init_device(w, cur)?;
                report(Node(cur), outcome);
                descend
            } else {
                // Scopes and other objects: devices may be below them.
                !matches!(
                    self.ns.obj(cur).ok().and_then(|o| self.heap.get(o).ok()),
                    Some(Object::Method(_))
                )
            };
            next = if descend {
                self.ns.first_child(cur)
            } else {
                None
            };
            let mut up = cur;
            while next.is_none() {
                if let Some(s) = self.ns.next_sibling(up) {
                    next = Some(s);
                    break;
                }
                match self.ns.parent(up) {
                    Some(p) if p != root => up = p,
                    _ => break,
                }
            }
        }
        Ok(())
    }

    /// Runs `_REG (space, connected)` in every scope that declares a region of `space`, except
    /// those §6.5.4 makes always available. `report` hears each scope and how its `_REG` ended.
    /// Returns how many ran.
    ///
    /// # Errors
    /// [`ErrorKind::Limit`] if the list of scopes cannot be held, or if the `_REG`s as a whole
    /// reached the limits' time or steps (the review of B2, B2-7); it stops there.
    pub fn connect_space(
        &mut self,
        space: Space,
        connected: bool,
        report: &mut dyn FnMut(Node, Result<()>),
    ) -> Result<usize> {
        if space == Space::SYSTEM_MEMORY || space == Space::SYSTEM_IO {
            return Ok(0);
        }
        let mut w = self.start_walk();
        let mut scopes: Vec<NodeId> = Vec::new();
        let mut r = self.region_scopes(space, &mut scopes);
        let mut ran = 0usize;
        if r.is_ok() {
            // The pass that found the scopes visited each node once.
            w.steps = w
                .steps
                .saturating_add(u64::try_from(self.node_count()).unwrap_or(u64::MAX));
            let args = [
                Value::Integer(u64::from(space.0)),
                Value::Integer(u64::from(connected)),
            ];
            for &scope in &scopes {
                match self.run_child(&mut w, scope, *b"_REG", &args) {
                    Ok(Some(outcome)) => {
                        ran = ran.saturating_add(1);
                        report(Node(scope), outcome.map(|_| ()));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        r = Err(self.walk_cut(e));
                        break;
                    }
                }
            }
        }
        mem::release(scopes, &mut self.budget);
        r.map(|()| ran)
    }

    /// The scopes that declare a region of `space`, each once, in definition order.
    fn region_scopes(&mut self, space: Space, out: &mut Vec<NodeId>) -> Result<()> {
        // Each node is a candidate scope once, in definition order, and each child is looked at
        // once as a possible region of the space: linear, and no scope listed twice.
        let root = crate::namespace::Namespace::root();
        let mut next = Some(root);
        while let Some(cur) = next {
            let mut child = self.ns.first_child(cur);
            let mut declares = false;
            while let Some(c) = child {
                if matches!(
                    self.ns.obj(c).ok().and_then(|o| self.heap.get(o).ok()),
                    Some(Object::Region(r)) if r.space == space.0
                ) {
                    declares = true;
                    break;
                }
                child = self.ns.next_sibling(c);
            }
            let root_bus = space == Space::PCI_CONFIG && self.ns.child(cur, *b"_BBN").is_some();
            if declares && !root_bus {
                mem::push(out, cur, &mut self.budget)?;
            }
            next = self.ns.first_child(cur);
            let mut up = cur;
            while next.is_none() && up != root {
                if let Some(s) = self.ns.next_sibling(up) {
                    next = Some(s);
                    break;
                }
                match self.ns.parent(up) {
                    Some(p) => up = p,
                    None => break,
                }
            }
        }
        Ok(())
    }
}
