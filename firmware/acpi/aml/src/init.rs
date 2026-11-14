//! Namespace initialization (ACPI 6.6 §6.3.7, §6.5.1 and Table 6.67) and `_REG` (§6.5.4).
//!
//! - [`Interpreter::initialize`] runs `\_SB._INI` if there is one, then walks the devices and
//!   processors depth first in definition order: each one's `_STA` (none means present and
//!   functioning), then its `_INI` if it is present, then its children if it is present or
//!   functioning. A failing `_STA` or `_INI` is reported, and the walk goes on with the next
//!   sibling, skipping what is below the device.
//! - [`Interpreter::connect_space`] runs `_REG (space, 1)` (or `0` on a disconnect) once in each
//!   scope that declares a region of that space, scopes in definition order. SystemMemory and SystemIO are always available and get
//!   none, nor do PCI_Config regions declared in a scope with a `_BBN` (a root bus), as §6.5.4
//!   says.
//!
//! Each `_STA`, `_INI` and `_REG` is an evaluation of its own, with its own limits. The walk itself
//! allocates nothing but what those evaluations do, and recurses nowhere: it follows the
//! namespace's parent and sibling links.

use alloc::vec::Vec;

use crate::Interpreter;
use crate::error::{Error, ErrorKind, Result};
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

    /// Runs the method or reads the object `seg` under `n`, if there is one.
    fn run_child(
        &mut self,
        n: NodeId,
        seg: [u8; 4],
        args: &[Value],
    ) -> Option<Result<Option<Value>>> {
        let c = self.ns.child(n, seg)?;
        Some(self.evaluate_node(Node(c), args))
    }

    /// One device: its outcome, and whether to walk into its children.
    fn init_device(&mut self, n: NodeId) -> (InitOutcome, bool) {
        let status = match self.run_child(n, *b"_STA", &[]) {
            None => 0x0F,
            Some(Ok(Some(Value::Integer(s)))) => s & 0xFFFF_FFFF,
            Some(Ok(_)) => return (InitOutcome::StaFailed(Error::new(ErrorKind::Type)), false),
            Some(Err(e)) => return (InitOutcome::StaFailed(e), false),
        };
        let present = status & PRESENT != 0;
        let functioning = status & FUNCTIONING != 0;
        if !present {
            return if functioning {
                (InitOutcome::ChildrenOnly { status }, true)
            } else {
                (InitOutcome::Absent { status }, false)
            };
        }
        match self.run_child(n, *b"_INI", &[]) {
            None => (InitOutcome::Initialized { status, ini: false }, true),
            Some(Ok(_)) => (InitOutcome::Initialized { status, ini: true }, true),
            Some(Err(error)) => (InitOutcome::IniFailed { status, error }, false),
        }
    }

    /// The initialization walk (Table 6.67). `report` hears every device and processor it
    /// reaches, and `\_SB` if it has an `_INI`.
    pub fn initialize(&mut self, report: &mut dyn FnMut(Node, InitOutcome)) {
        let root = crate::namespace::Namespace::root();
        if let Some(sb) = self.ns.child(root, *b"_SB_")
            && let Some(r) = self.run_child(sb, *b"_INI", &[])
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
            let descend = if self.is_device(cur) {
                let (outcome, descend) = self.init_device(cur);
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
    }

    /// Runs `_REG (space, connected)` in every scope that declares a region of `space`, except
    /// those §6.5.4 makes always available. `report` hears each scope and how its `_REG` ended.
    /// Returns how many ran.
    ///
    /// # Errors
    /// [`ErrorKind::Limit`] if the list of scopes cannot be held.
    pub fn connect_space(
        &mut self,
        space: Space,
        connected: bool,
        report: &mut dyn FnMut(Node, Result<()>),
    ) -> Result<usize> {
        if space == Space::SYSTEM_MEMORY || space == Space::SYSTEM_IO {
            return Ok(0);
        }
        let mut scopes: Vec<NodeId> = Vec::new();
        let r = self.region_scopes(space, &mut scopes);
        let mut ran = 0usize;
        if r.is_ok() {
            let args = [
                Value::Integer(u64::from(space.0)),
                Value::Integer(u64::from(connected)),
            ];
            for &scope in &scopes {
                if let Some(outcome) = self.run_child(scope, *b"_REG", &args) {
                    ran = ran.saturating_add(1);
                    report(Node(scope), outcome.map(|_| ()));
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
