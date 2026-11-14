//! Objects and values (ACPI 6.6 §19.3.5, Table 19.5), and the heap that holds them.
//!
//! Objects live in an arena and are named by handles that carry a generation: a handle to a freed
//! slot is refused ([`ErrorKind::Stale`]), never followed. Nothing holds an object by `Box`, so
//! freeing a deep package never recurses. A mark-and-sweep collector reclaims what is unreachable
//! from the roots its caller names; it allocates nothing, because the mark stack is reserved one
//! entry per slot as the slots are made.

use alloc::vec::Vec;

use crate::error::{ErrorKind, Result, err};
use crate::mem::{self, Budget};

/// "No index": the end of an intrusive list.
pub(crate) const NONE: u32 = u32::MAX;

/// A handle to an object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObjRef {
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

/// A handle to a namespace node. Its generation is 64 bits: a node slot reused for ever never
/// brings an old handle back (the independent review of B1, F8). An object handle's 32 bits
/// suffice, since the collector traces every holder of one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NodeId {
    pub(crate) index: u32,
    pub(crate) generation: u64,
}

/// A value as the interpreter moves it: in a local, an argument, a package element, an operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Val {
    /// Uninitialized, or no value.
    None,
    /// An integer, held directly.
    Int(u64),
    /// Any other object, shared: a store copies it, reading does not.
    Obj(ObjRef),
    /// An object reference (`RefOf`, `Index`, `CondRefOf`).
    Ref(RefTarget),
}

/// What an object reference points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefTarget {
    /// A named object.
    Node(NodeId),
    /// A local of the method activation numbered `call`.
    Local { call: u64, slot: u8 },
    /// An argument of the method activation numbered `call`.
    Arg { call: u64, slot: u8 },
    /// A package element (`Index` of a package).
    Elem { pkg: ObjRef, index: u32 },
    /// One byte of a buffer or string, as an 8-bit buffer field (`Index` of a buffer or string).
    Byte { obj: ObjRef, index: u32 },
}

/// The kinds of named scope object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    /// A predefined scope with no type: the root, `\_SB`, `\_GPE`, `\_PR`, `\_SI`, `\_TZ`.
    Typeless,
    /// `Device`.
    Device,
    /// `Processor` (0x5B 0x83): reserved by ACPI 6.6, accepted on the coordinating session's
    /// ruling of 2026-11-14 because deployed firmware and QEMU use it.
    Processor { id: u8, pblk: u32, pblk_len: u8 },
    /// `PowerResource`.
    PowerResource { level: u8, order: u16 },
    /// `ThermalZone`.
    ThermalZone,
}

/// An operation region (`OperationRegion`, `DataTableRegion`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub(crate) space: u8,
    pub(crate) offset: u64,
    pub(crate) length: u64,
    pub(crate) node: NodeId,
    /// A PCI_Config region's function, found at its first access (`region.rs`).
    pub(crate) pci: Option<crate::host::PciAddress>,
}

/// Where a field unit's bits are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldKind {
    /// In an operation region (`Field`).
    Region(NodeId),
    /// Behind an index and a data field unit (`IndexField`).
    Index { index: NodeId, data: NodeId },
    /// In a region, after `value` is written to the bank field unit (`BankField`).
    Bank {
        region: NodeId,
        bank: NodeId,
        value: u64,
    },
}

/// A field unit's connection (`Connection`), for GPIO and serial-bus regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Connection {
    None,
    /// A named object holding a resource template, resolved when the field is accessed (so a
    /// field may name a template defined after it).
    Name(NameElem),
    /// A resource template given in the field list.
    Buffer(ObjRef),
}

/// A field unit (`Field`, `IndexField`, `BankField`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FieldUnit {
    pub(crate) kind: FieldKind,
    pub(crate) bit_offset: u64,
    pub(crate) bit_len: u64,
    /// The access type, bits 0–3 of the flags (Any, Byte, Word, DWord, QWord, Buffer).
    pub(crate) access: u8,
    /// The access attribute byte.
    pub(crate) attrib: u8,
    /// The access length, for the attributes that take one.
    pub(crate) access_len: u8,
    /// The lock rule: take the Global Lock.
    pub(crate) lock: bool,
    /// The update rule: 0 Preserve, 1 WriteAsOnes, 2 WriteAsZeros.
    pub(crate) update: u8,
    pub(crate) connection: Connection,
}

/// A buffer field (`Create…Field`), over a buffer or a string object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BufferField {
    pub(crate) source: ObjRef,
    pub(crate) bit_offset: u64,
    pub(crate) bit_len: u64,
}

/// A control method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Method {
    pub(crate) table: u32,
    /// The body: `start..end` in the table's term list.
    pub(crate) start: u32,
    pub(crate) end: u32,
    /// Method flags: argument count in bits 0–2, serialized in 3, sync level in 4–7.
    pub(crate) flags: u8,
    /// The method's own node: its scope (§19.6.85).
    pub(crate) node: NodeId,
}

impl Method {
    pub(crate) const fn argc(&self) -> u8 {
        self.flags & 7
    }
}

/// A mutex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mutex {
    pub(crate) sync: u8,
    pub(crate) depth: u32,
    /// `\_GL`: also takes the Global Lock.
    pub(crate) global: bool,
}

/// A name string kept as a package element, resolved each time it is read (§19.6.102).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NameElem {
    /// The scope the package was built in.
    pub(crate) scope: NodeId,
    pub(crate) table: u32,
    /// Where the name string is in the table's term list.
    pub(crate) at: u32,
}

/// An object.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Object {
    /// A free slot.
    Free,
    Uninitialized,
    Integer(u64),
    /// The characters, without the terminating NUL.
    String(Vec<u8>),
    Buffer(Vec<u8>),
    Package(Vec<Val>),
    Reference(RefTarget),
    BufferField(BufferField),
    FieldUnit(FieldUnit),
    Region(Region),
    Method(Method),
    /// `\_OSI`, built in.
    Osi,
    Mutex(Mutex),
    /// An event and its pending signal count.
    Event(u64),
    /// A scope: device, processor, power resource, thermal zone or a typeless predefined scope.
    Scope {
        kind: ScopeKind,
        node: NodeId,
    },
    /// `Alias`: another node.
    Alias(NodeId),
    NameElem(NameElem),
    /// A package being deep-copied, until its elements are in: the package it copies.
    Placeholder(ObjRef),
}

/// ACPI 6.6 Table 19.36's type codes.
pub(crate) mod types {
    pub(crate) const UNINITIALIZED: u64 = 0;
    pub(crate) const INTEGER: u64 = 1;
    pub(crate) const STRING: u64 = 2;
    pub(crate) const BUFFER: u64 = 3;
    pub(crate) const PACKAGE: u64 = 4;
    pub(crate) const FIELD_UNIT: u64 = 5;
    pub(crate) const DEVICE: u64 = 6;
    pub(crate) const EVENT: u64 = 7;
    pub(crate) const METHOD: u64 = 8;
    pub(crate) const MUTEX: u64 = 9;
    pub(crate) const REGION: u64 = 10;
    pub(crate) const POWER_RESOURCE: u64 = 11;
    /// Reserved by ACPI 6.6's Table 19.36, still "Processor" in its Table 19.31 (E7).
    pub(crate) const PROCESSOR: u64 = 12;
    pub(crate) const THERMAL_ZONE: u64 = 13;
    pub(crate) const BUFFER_FIELD: u64 = 14;
    pub(crate) const DEBUG: u64 = 16;
}

impl Object {
    /// The type code (Table 19.36). References, aliases and name elements are resolved by the
    /// caller first; here they count as uninitialized.
    pub(crate) const fn type_code(&self) -> u64 {
        match self {
            Self::Integer(_) => types::INTEGER,
            Self::String(_) => types::STRING,
            Self::Buffer(_) => types::BUFFER,
            Self::Package(_) => types::PACKAGE,
            Self::FieldUnit(_) => types::FIELD_UNIT,
            Self::Event(_) => types::EVENT,
            Self::Method(_) | Self::Osi => types::METHOD,
            Self::Mutex(_) => types::MUTEX,
            Self::Region(_) => types::REGION,
            Self::BufferField(_) => types::BUFFER_FIELD,
            Self::Scope { kind, .. } => match kind {
                ScopeKind::Typeless => types::UNINITIALIZED,
                ScopeKind::Device => types::DEVICE,
                ScopeKind::Processor { .. } => types::PROCESSOR,
                ScopeKind::PowerResource { .. } => types::POWER_RESOURCE,
                ScopeKind::ThermalZone => types::THERMAL_ZONE,
            },
            Self::Free
            | Self::Uninitialized
            | Self::Reference(_)
            | Self::Alias(_)
            | Self::NameElem(_)
            | Self::Placeholder(_) => types::UNINITIALIZED,
        }
    }

    /// Table 19.31's name for a type that concatenates as a string.
    pub(crate) const fn type_text(code: u64) -> &'static [u8] {
        match code {
            types::PACKAGE => b"[Package]",
            types::FIELD_UNIT => b"[Field]",
            types::DEVICE => b"[Device]",
            types::EVENT => b"[Event]",
            types::METHOD => b"[Control Method]",
            types::MUTEX => b"[Mutex]",
            types::REGION => b"[Operation Region]",
            types::POWER_RESOURCE => b"[Power Resource]",
            types::PROCESSOR => b"[Processor]",
            types::THERMAL_ZONE => b"[Thermal Zone]",
            types::BUFFER_FIELD => b"[Buffer Field]",
            types::DEBUG => b"[Debug Object]",
            _ => b"[Uninitialized Object]",
        }
    }

    /// Whether the object is data: an integer, string, buffer or package (or a field that reads as
    /// one), as opposed to a device, method, mutex …
    pub(crate) const fn is_data(&self) -> bool {
        matches!(
            self,
            Self::Integer(_)
                | Self::String(_)
                | Self::Buffer(_)
                | Self::Package(_)
                | Self::BufferField(_)
                | Self::FieldUnit(_)
                | Self::Reference(_)
                | Self::Uninitialized
        )
    }
}

/// The bytes an object's payload holds.
pub(crate) fn payload_bytes(obj: &Object) -> usize {
    match obj {
        Object::String(v) | Object::Buffer(v) => v.capacity(),
        Object::Package(v) => v.capacity().saturating_mul(core::mem::size_of::<Val>()),
        _ => 0,
    }
}

/// Frees an object's payload, crediting its bytes.
pub(crate) fn release_object(obj: Object, budget: &mut Budget) {
    match obj {
        Object::String(v) | Object::Buffer(v) => mem::release(v, budget),
        Object::Package(v) => mem::release(v, budget),
        _ => {}
    }
}

struct Slot {
    generation: u32,
    mark: bool,
    next_free: u32,
    obj: Object,
}

/// The object arena.
pub(crate) struct Heap {
    slots: Vec<Slot>,
    free: u32,
    /// The collector's mark stack: its capacity is kept at least the number of slots.
    stack: Vec<u32>,
    live: usize,
}

impl core::fmt::Debug for Heap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Heap")
            .field("slots", &self.slots.len())
            .field("live", &self.live)
            .finish()
    }
}

fn index(i: u32) -> usize {
    usize::try_from(i).unwrap_or(usize::MAX)
}

impl Heap {
    pub(crate) const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: NONE,
            stack: Vec::new(),
            live: 0,
        }
    }

    /// Objects alive.
    pub(crate) const fn live(&self) -> usize {
        self.live
    }

    /// Slots, free or not: what a collection reads.
    pub(crate) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// The bytes the heap holds, counted as `mem.rs` charges them: its two vectors' capacities
    /// and every object's payload.
    pub(crate) fn bytes(&self) -> usize {
        let mut n = self
            .slots
            .capacity()
            .saturating_mul(core::mem::size_of::<Slot>())
            .saturating_add(self.stack.capacity().saturating_mul(4));
        for s in &self.slots {
            n = n.saturating_add(payload_bytes(&s.obj));
        }
        n
    }

    /// Stores `obj`, whose payload its maker has charged. On failure the payload is released.
    pub(crate) fn alloc(&mut self, obj: Object, budget: &mut Budget) -> Result<ObjRef> {
        if self.free != NONE {
            let i = self.free;
            if let Some(slot) = self.slots.get_mut(index(i)) {
                self.free = slot.next_free;
                slot.next_free = NONE;
                slot.mark = false;
                slot.obj = obj;
                self.live = self.live.saturating_add(1);
                return Ok(ObjRef {
                    index: i,
                    generation: slot.generation,
                });
            }
            self.free = NONE;
        }
        let i = match u32::try_from(self.slots.len()) {
            Ok(i) if i != NONE => i,
            _ => {
                release_object(obj, budget);
                return err(ErrorKind::Limit(crate::Limit::Memory));
            }
        };
        let room = self.slots.len().saturating_add(1);
        if let Err(e) = mem::reserve(&mut self.stack, room, budget)
            .and_then(|()| mem::reserve(&mut self.slots, 1, budget))
        {
            // Nothing moved: the payload is credited back (the review of B1, F2).
            release_object(obj, budget);
            return Err(e);
        }
        let slot = Slot {
            generation: 0,
            mark: false,
            next_free: NONE,
            obj,
        };
        // Cannot fail: the room was reserved above.
        mem::push(&mut self.slots, slot, budget)?;
        self.live = self.live.saturating_add(1);
        Ok(ObjRef {
            index: i,
            generation: 0,
        })
    }

    pub(crate) fn get(&self, r: ObjRef) -> Result<&Object> {
        match self.slots.get(index(r.index)) {
            Some(s) if s.generation == r.generation && !matches!(s.obj, Object::Free) => Ok(&s.obj),
            _ => err(ErrorKind::Stale),
        }
    }

    pub(crate) fn get_mut(&mut self, r: ObjRef) -> Result<&mut Object> {
        match self.slots.get_mut(index(r.index)) {
            Some(s) if s.generation == r.generation && !matches!(s.obj, Object::Free) => {
                Ok(&mut s.obj)
            }
            _ => err(ErrorKind::Stale),
        }
    }

    /// Replaces the object `r` names with `new`, releasing the old one's payload.
    pub(crate) fn replace(&mut self, r: ObjRef, new: Object, budget: &mut Budget) -> Result<()> {
        match self.get_mut(r) {
            Ok(slot) => {
                let old = core::mem::replace(slot, new);
                release_object(old, budget);
                Ok(())
            }
            Err(e) => {
                release_object(new, budget);
                Err(e)
            }
        }
    }

    // --- The collector -------------------------------------------------------------------

    /// Starts a collection: every mark cleared.
    pub(crate) fn clear_marks(&mut self) {
        for s in &mut self.slots {
            s.mark = false;
        }
        self.stack.clear();
    }

    /// Marks a root. Returns `false` if the mark stack could not take it, in which case the
    /// collection must be abandoned (it cannot happen: the stack's capacity is one per slot).
    pub(crate) fn mark(&mut self, v: Val, budget: &mut Budget) -> bool {
        match obj_of(v) {
            Some(r) => self.mark_obj(r, budget),
            None => true,
        }
    }

    fn mark_obj(&mut self, r: ObjRef, budget: &mut Budget) -> bool {
        match self.slots.get_mut(index(r.index)) {
            Some(s)
                if s.generation == r.generation && !s.mark && !matches!(s.obj, Object::Free) =>
            {
                s.mark = true;
                mem::push(&mut self.stack, r.index, budget).is_ok()
            }
            _ => true,
        }
    }

    /// Marks everything reachable from the marked roots. `false`: abandon the collection.
    pub(crate) fn trace(&mut self, budget: &mut Budget) -> bool {
        enum Out {
            Elements(usize),
            One(Val),
            Nothing,
        }
        while let Some(i) = self.stack.pop() {
            let out = match self.slots.get(index(i)).map(|s| &s.obj) {
                Some(Object::Package(elems)) => Out::Elements(elems.len()),
                Some(Object::Reference(t)) => Out::One(Val::Ref(*t)),
                Some(Object::BufferField(f)) => Out::One(Val::Obj(f.source)),
                Some(Object::FieldUnit(FieldUnit {
                    connection: Connection::Buffer(r),
                    ..
                })) => Out::One(Val::Obj(*r)),
                Some(Object::Placeholder(r)) => Out::One(Val::Obj(*r)),
                _ => Out::Nothing,
            };
            let ok = match out {
                Out::Elements(n) => {
                    let mut ok = true;
                    for k in 0..n {
                        let e = match self.slots.get(index(i)).map(|s| &s.obj) {
                            Some(Object::Package(v)) => v.get(k).copied().unwrap_or(Val::None),
                            _ => Val::None,
                        };
                        ok &= self.mark(e, budget);
                    }
                    ok
                }
                Out::One(v) => self.mark(v, budget),
                Out::Nothing => true,
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// Frees every unmarked object. Returns how many.
    pub(crate) fn sweep(&mut self, budget: &mut Budget) -> usize {
        let mut freed: usize = 0;
        let mut i: u32 = 0;
        let mut head = self.free;
        for s in &mut self.slots {
            if !s.mark && !matches!(s.obj, Object::Free) {
                let old = core::mem::replace(&mut s.obj, Object::Free);
                release_object(old, budget);
                s.generation = s.generation.wrapping_add(1);
                s.next_free = head;
                head = i;
                freed = freed.saturating_add(1);
            }
            s.mark = false;
            i = i.wrapping_add(1);
        }
        self.free = head;
        self.live = self.live.saturating_sub(freed);
        freed
    }
}

/// The object a value keeps alive, if any.
pub(crate) const fn obj_of(v: Val) -> Option<ObjRef> {
    match v {
        Val::Obj(r) | Val::Ref(RefTarget::Elem { pkg: r, .. } | RefTarget::Byte { obj: r, .. }) => {
            Some(r)
        }
        _ => None,
    }
}
