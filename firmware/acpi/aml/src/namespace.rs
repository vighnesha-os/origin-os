//! The namespace (ACPI 6.6 §5.3): a tree of named nodes, each holding an object.
//!
//! Nodes live in an arena, linked to their parent, first and last child and both siblings, and
//! named by handles with a generation. Removing a subtree allocates nothing and is linear in its
//! size: a walk down to a leaf, which is unlinked in constant time, then on from its parent; freed
//! slots are chained through their sibling link. Finding a child compares names one sibling at a
//! time, and every comparison is counted ([`Namespace::take_work`]) so that the engine charges it
//! to the step limit (the review of B1, F1). Each node's creator (a table being
//! loaded, or a method activation) keeps a list of the handles it made, so a failed load or a
//! returning method removes exactly what it created; a handle whose node has already gone is
//! skipped by its generation.

use alloc::vec::Vec;

use crate::error::{ErrorKind, Result, err};
use crate::mem::{self, Budget};
use crate::name::{Seg, Segs};
use crate::object::{NONE, NodeId, ObjRef};

/// Who made a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// The interpreter itself: the root, the predefined scopes and objects.
    Builtin,
    /// The table numbered so, while it loaded.
    Table(u32),
    /// The method activation numbered so (dynamic; removed when it returns).
    Call(u64),
}

#[derive(Debug)]
struct Node {
    name: Seg,
    generation: u64,
    live: bool,
    parent: u32,
    first: u32,
    last: u32,
    next: u32,
    prev: u32,
    obj: ObjRef,
    owner: Owner,
}

/// The namespace.
#[derive(Debug)]
pub(crate) struct Namespace {
    nodes: Vec<Node>,
    free: u32,
    live: usize,
    /// Sibling comparisons since the engine last took the count.
    work: core::cell::Cell<u64>,
}

fn index(i: u32) -> usize {
    usize::try_from(i).unwrap_or(usize::MAX)
}

/// The most aliases followed in a row; a longer chain is refused as a cycle.
const ALIAS_HOPS: u32 = 16;

impl Namespace {
    /// A namespace holding the root alone, whose object is `root_obj`.
    pub(crate) fn new(root_obj: ObjRef, budget: &mut Budget) -> Result<Self> {
        let mut nodes = Vec::new();
        mem::push(
            &mut nodes,
            Node {
                name: *b"\\___",
                generation: 0,
                live: true,
                parent: NONE,
                first: NONE,
                last: NONE,
                next: NONE,
                prev: NONE,
                obj: root_obj,
                owner: Owner::Builtin,
            },
            budget,
        )?;
        Ok(Self {
            nodes,
            free: NONE,
            live: 1,
            work: core::cell::Cell::new(0),
        })
    }

    pub(crate) const fn root() -> NodeId {
        NodeId {
            index: 0,
            generation: 0,
        }
    }

    /// Nodes alive.
    pub(crate) const fn live(&self) -> usize {
        self.live
    }

    /// The bytes the namespace holds, as `mem.rs` charges them.
    pub(crate) fn bytes(&self) -> usize {
        self.nodes
            .capacity()
            .saturating_mul(core::mem::size_of::<Node>())
    }

    fn node(&self, id: NodeId) -> Result<&Node> {
        match self.nodes.get(index(id.index)) {
            Some(n) if n.live && n.generation == id.generation => Ok(n),
            _ => err(ErrorKind::Stale),
        }
    }

    fn at(&self, i: u32) -> Option<&Node> {
        self.nodes.get(index(i)).filter(|n| n.live)
    }

    fn id(&self, i: u32) -> Option<NodeId> {
        self.at(i).map(|n| NodeId {
            index: i,
            generation: n.generation,
        })
    }

    /// Whether `id` names a live node.
    pub(crate) fn exists(&self, id: NodeId) -> bool {
        self.node(id).is_ok()
    }

    pub(crate) fn obj(&self, id: NodeId) -> Result<ObjRef> {
        Ok(self.node(id)?.obj)
    }

    /// Points `id` at another object: `CopyObject` to a name (the review of B1, F13).
    pub(crate) fn set_obj(&mut self, id: NodeId, obj: ObjRef) -> Result<()> {
        match self.nodes.get_mut(index(id.index)) {
            Some(n) if n.live && n.generation == id.generation => {
                n.obj = obj;
                Ok(())
            }
            _ => err(ErrorKind::Stale),
        }
    }

    pub(crate) fn name(&self, id: NodeId) -> Result<Seg> {
        Ok(self.node(id)?.name)
    }

    pub(crate) fn owner(&self, id: NodeId) -> Result<Owner> {
        Ok(self.node(id)?.owner)
    }

    pub(crate) fn parent(&self, id: NodeId) -> Option<NodeId> {
        let p = self.node(id).ok()?.parent;
        self.id(p)
    }

    pub(crate) fn first_child(&self, id: NodeId) -> Option<NodeId> {
        let c = self.node(id).ok()?.first;
        self.id(c)
    }

    pub(crate) fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        let s = self.node(id).ok()?.next;
        self.id(s)
    }

    /// Sibling comparisons made since the last call: the engine charges them as steps.
    /// Counts `n` units of work done on the namespace by its caller.
    pub(crate) fn note_work(&self, n: u64) {
        self.work.set(self.work.get().saturating_add(n));
    }

    pub(crate) fn take_work(&self) -> u64 {
        self.work.replace(0)
    }

    /// The child of `parent` named `seg`. Each sibling compared counts as work.
    pub(crate) fn child(&self, parent: NodeId, seg: Seg) -> Option<NodeId> {
        let mut c = self.node(parent).ok()?.first;
        while let Some(n) = self.at(c) {
            self.work.set(self.work.get().saturating_add(1));
            if n.name == seg {
                return Some(NodeId {
                    index: c,
                    generation: n.generation,
                });
            }
            c = n.next;
        }
        None
    }

    /// Every live node, in no particular order: the collector's roots.
    pub(crate) fn objects(&self) -> impl Iterator<Item = ObjRef> + '_ {
        self.nodes.iter().filter(|n| n.live).map(|n| n.obj)
    }

    /// Makes `seg` a child of `parent`, holding `obj`, and adds it to its creator's `list`.
    pub(crate) fn create(
        &mut self,
        parent: NodeId,
        seg: Seg,
        obj: ObjRef,
        owner: Owner,
        list: &mut Vec<NodeId>,
        budget: &mut Budget,
    ) -> Result<NodeId> {
        self.node(parent)?;
        if self.child(parent, seg).is_some() {
            return err(ErrorKind::Duplicate);
        }
        // Room on the list first, so a node never exists that its creator cannot remove.
        mem::reserve(list, 1, budget)?;
        let node = Node {
            name: seg,
            generation: 0,
            live: true,
            parent: parent.index,
            first: NONE,
            last: NONE,
            next: NONE,
            prev: NONE,
            obj,
            owner,
        };
        let i = if self.free == NONE {
            let i = u32::try_from(self.nodes.len())
                .ok()
                .filter(|&i| i != NONE)
                .ok_or(ErrorKind::Limit(crate::Limit::Memory))?;
            mem::push(&mut self.nodes, node, budget)?;
            i
        } else {
            let i = self.free;
            let slot = self.nodes.get_mut(index(i)).ok_or(ErrorKind::Stale)?;
            self.free = slot.next;
            let generation = slot.generation;
            *slot = Node { generation, ..node };
            i
        };
        let generation = self.nodes.get(index(i)).map_or(0, |n| n.generation);
        // Link it last among its siblings, so the namespace keeps definition order.
        let last = self.nodes.get(index(parent.index)).map_or(NONE, |p| p.last);
        if let Some(prev) = self.nodes.get_mut(index(last)).filter(|n| n.live) {
            prev.next = i;
            if let Some(me) = self.nodes.get_mut(index(i)) {
                me.prev = last;
            }
        } else if let Some(p) = self.nodes.get_mut(index(parent.index)) {
            p.first = i;
        }
        if let Some(p) = self.nodes.get_mut(index(parent.index)) {
            p.last = i;
        }
        self.live = self.live.saturating_add(1);
        let id = NodeId {
            index: i,
            generation,
        };
        // Cannot fail: the room was reserved above.
        mem::push(list, id, budget)?;
        Ok(id)
    }

    /// Removes `id` and everything below it. Allocates nothing, and takes time linear in the
    /// subtree: each move either goes down to a first child or unlinks a leaf and goes back up to
    /// its parent.
    pub(crate) fn remove(&mut self, id: NodeId) {
        if self.node(id).is_err() || id.index == 0 {
            return;
        }
        let mut cur = id.index;
        loop {
            let first = self.at(cur).map_or(NONE, |n| n.first);
            if self.at(first).is_some() {
                cur = first;
                continue;
            }
            let parent = self.at(cur).map_or(NONE, |n| n.parent);
            self.unlink(cur);
            if cur == id.index {
                return;
            }
            cur = parent;
        }
    }

    /// Unlinks the childless node `i` from its parent and frees it, in constant time.
    fn unlink(&mut self, i: u32) {
        let Some((parent, prev, next)) = self.at(i).map(|n| (n.parent, n.prev, n.next)) else {
            return;
        };
        if let Some(p) = self.nodes.get_mut(index(prev)).filter(|n| n.live) {
            p.next = next;
        } else if let Some(p) = self.nodes.get_mut(index(parent)) {
            p.first = next;
        }
        if let Some(n) = self.nodes.get_mut(index(next)).filter(|n| n.live) {
            n.prev = prev;
        } else if let Some(p) = self.nodes.get_mut(index(parent)) {
            p.last = prev;
        }
        let free = self.free;
        if let Some(n) = self.nodes.get_mut(index(i)) {
            n.live = false;
            n.generation = n.generation.wrapping_add(1);
            n.first = NONE;
            n.last = NONE;
            n.prev = NONE;
            n.next = free;
            self.free = i;
            self.live = self.live.saturating_sub(1);
        }
    }

    /// Removes every node on a creator's `list` that is still there, with what is below it, newest
    /// first.
    pub(crate) fn remove_list(&mut self, list: &[NodeId]) {
        for &id in list.iter().rev() {
            self.remove(id);
        }
    }

    /// Follows aliases from `id`, to the node they end at.
    pub(crate) fn unalias(
        &self,
        id: NodeId,
        alias_of: impl Fn(ObjRef) -> Option<NodeId>,
    ) -> Result<NodeId> {
        let mut cur = id;
        for _ in 0..ALIAS_HOPS {
            match alias_of(self.obj(cur)?) {
                Some(next) => cur = next,
                None => return Ok(cur),
            }
        }
        err(ErrorKind::NotFound)
    }

    /// Where a path starts: the root, or `scope` raised by its parent prefixes.
    fn base(&self, scope: NodeId, path: &impl Segs) -> Result<NodeId> {
        if path.root() {
            return Ok(Self::root());
        }
        // Each parent prefix was a byte parsed, and is a scope gone up: a unit of work each (the
        // review of B1, A1).
        self.work
            .set(self.work.get().saturating_add(u64::from(path.parents())));
        let mut cur = scope;
        for _ in 0..path.parents() {
            cur = self.parent(cur).ok_or(ErrorKind::NotFound)?;
        }
        Ok(cur)
    }

    /// Finds `path` from `scope`: by the search rules for a single relative segment (§5.3),
    /// exactly otherwise. Aliases on the way are followed by `alias_of`. `Ok(None)` if absent.
    pub(crate) fn lookup(
        &self,
        scope: NodeId,
        path: &impl Segs,
        alias_of: impl Fn(ObjRef) -> Option<NodeId> + Copy,
    ) -> Result<Option<NodeId>> {
        self.node(scope)?;
        if path.searched() {
            let seg = path.seg(0)?;
            let mut cur = Some(scope);
            while let Some(s) = cur {
                if let Some(found) = self.child(s, seg) {
                    return Ok(Some(found));
                }
                cur = self.parent(s);
            }
            return Ok(None);
        }
        let Ok(mut cur) = self.base(scope, path) else {
            return Ok(None);
        };
        for i in 0..path.count() {
            cur = self.unalias(cur, alias_of)?;
            match self.child(cur, path.seg(i)?) {
                Some(next) => cur = next,
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    /// Where `path`, relative to `scope`, would be created: its parent (which must exist) and its
    /// last segment. The search rules do not apply.
    pub(crate) fn place(
        &self,
        scope: NodeId,
        path: &impl Segs,
        alias_of: impl Fn(ObjRef) -> Option<NodeId> + Copy,
    ) -> Result<(NodeId, Seg)> {
        let count = path.count();
        let Some(last) = count.checked_sub(1) else {
            return err(ErrorKind::Name);
        };
        let mut cur = self.base(scope, path)?;
        for i in 0..last {
            cur = self.unalias(cur, alias_of)?;
            cur = self.child(cur, path.seg(i)?).ok_or(ErrorKind::NotFound)?;
        }
        cur = self.unalias(cur, alias_of)?;
        Ok((cur, path.seg(last)?))
    }

    /// The segments from the root to `id`, appended to `out` (root first).
    pub(crate) fn path(&self, id: NodeId, out: &mut Vec<Seg>, budget: &mut Budget) -> Result<()> {
        let start = out.len();
        let mut cur = self.node(id)?;
        let mut i = id.index;
        while i != 0 {
            mem::push(out, cur.name, budget)?;
            i = cur.parent;
            cur = self.at(i).ok_or(ErrorKind::Stale)?;
        }
        if let Some(tail) = out.get_mut(start..) {
            tail.reverse();
        }
        Ok(())
    }

    /// `id`'s path written into `out`, allocating nothing (for a tracing host): the segments
    /// used, or `None` if `id` is stale or deeper than `out`.
    pub(crate) fn path_into<'o>(&self, id: NodeId, out: &'o mut [Seg]) -> Option<&'o [Seg]> {
        let mut depth = 0usize;
        let mut cur = self.node(id).ok()?;
        let mut i = id.index;
        while i != 0 {
            depth = depth.checked_add(1)?;
            i = cur.parent;
            cur = self.at(i)?;
        }
        let used = out.get_mut(..depth)?;
        let mut cur = self.node(id).ok()?;
        for slot in used.iter_mut().rev() {
            *slot = cur.name;
            cur = self.at(cur.parent)?;
        }
        Some(used)
    }
}
