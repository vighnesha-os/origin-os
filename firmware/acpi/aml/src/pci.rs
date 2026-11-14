//! Which PCI function a PCI_Config region names (ACPI 6.6 §5.5.2.4; `_HID`, `_CID`, `_SEG`,
//! `_BBN`, `_ADR`), found by frames on the engine's own stack (the review of B2, B2-1).
//!
//! A region's function is found before its first access, never inside it. A field read or write
//! first looks through the field, and its bank and index registers, for a PCI_Config region whose
//! function is not yet known ([`Interpreter::unresolved`]). If there is one, nothing has reached
//! the host yet: the step that wanted the access is put back as it was, and a [`Resolve`] frame is
//! pushed above it. That frame asks each object the walk needs in turn; a method among them runs
//! as an ordinary call on the frame stack and hands its value back to the frame, as it would to an
//! operator. When the function is known it is kept in the region, the frame goes, and the step
//! runs again. No Rust frame is held across a method, so the walk's depth costs heap frames, under
//! the limits, and never the caller's stack.
//!
//! A walk that needs the same region's access again (an `_ADR` that reads a field of the region
//! it names) is [`ErrorKind::AddressCycle`], never a recursion.
//!
//! **The walk follows devices** (the review of B2, B2-2). The function is the nearest `Device`
//! above the region: a region declared in a method, a power resource or a thermal zone names the
//! device around it. Going up from that device, each `Device` is asked whether it is a root bridge
//! (`_HID` or `_CID` PNP0A03 or PNP0A08); other scopes are passed over. Going down from the root
//! bridge, each `Device` between it and the function with an `_ADR` is a bridge, whose secondary
//! bus is read; a device with none is passed over. The function's own `_ADR` is required: a
//! device with none is [`ErrorKind::NotFound`], never function 0. With no root bridge above, the
//! segment and bus are 0 and there are no bridges.

use crate::Interpreter;
use crate::engine::Frame;
use crate::error::{Error, ErrorKind, Result, err};
use crate::host::{BridgeBus, Host, PciAddress, Space, Trace};
use crate::name::Seg;
use crate::object::{FieldKind, NodeId, Object, ScopeKind, Val};
use crate::region::FIELD_DEPTH;

/// `PNP0A03` and `PNP0A08` as EISA IDs: a PCI and a PCI Express root bridge.
const ROOT_BRIDGES: [u64; 2] = [0x030A_D041, 0x080A_D041];

/// A PCI `_ADR`'s device (bits 31:16) and function (bits 15:0), each in range: device 0 to 31,
/// function 0 to 7. Anything else, `0xFFFF` ("all functions") among it, names no one function
/// and is refused (the review of B2, B2-9).
fn device_function(adr: u64) -> Result<(u8, u8)> {
    let device = u8::try_from((adr >> 16) & 0xFFFF).ok().filter(|d| *d <= 31);
    let function = u8::try_from(adr & 0xFFFF).ok().filter(|f| *f <= 7);
    match (device, function) {
        (Some(d), Some(f)) if adr >> 32 == 0 => Ok((d, f)),
        _ => err(ErrorKind::Range),
    }
}

/// What the walk asks next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Is `at` a root bridge, by its `_HID`?
    Hid,
    /// By its `_CID`?
    Cid,
    /// The root bridge's `_SEG`.
    Seg,
    /// The root bridge's `_BBN`.
    Bbn,
    /// The `_ADR` of the next bridge below `at`, toward the device.
    BridgeAdr,
    /// The device's own `_ADR`.
    DeviceAdr,
}

/// A walk in progress, as a frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Resolve {
    /// The region whose function is wanted.
    pub(crate) region: NodeId,
    /// The node whose `_ADR` names the function.
    device: NodeId,
    phase: Phase,
    /// Going up: the node asked about. Going down: the last bridge passed.
    at: NodeId,
    /// The node the current question is about.
    asked: NodeId,
    root: Option<NodeId>,
    segment: u64,
    bus: u64,
    /// An answer a method handed back, not yet used.
    pub(crate) got: Option<Val>,
}

/// What asking a node for `seg` gave.
enum Asked {
    Absent,
    Value(Val),
    /// A method, now running above the frame; its value comes back to the frame.
    Called,
}

impl<H: Host> Interpreter<H> {
    /// The first PCI_Config region whose function is not known that reading or writing the field
    /// unit `node` would touch, through its bank and index registers. Bounded by the chain's
    /// depth.
    pub(crate) fn unresolved(&self, node: NodeId) -> Result<Option<NodeId>> {
        let mut stack = [(node, 0u32); 2 * (FIELD_DEPTH as usize + 2)];
        let mut top = 1usize;
        while let Some(t) = top.checked_sub(1) {
            top = t;
            let (n, depth) = *stack.get(t).ok_or(ErrorKind::Range)?;
            if depth > FIELD_DEPTH {
                return err(ErrorKind::Range);
            }
            let Object::FieldUnit(f) = self.heap.get(self.ns.obj(n)?)? else {
                return err(ErrorKind::WrongObject);
            };
            let next = depth.saturating_add(1);
            let (region, more) = match f.kind {
                FieldKind::Region(r) => (Some(r), [None, None]),
                FieldKind::Bank { region, bank, .. } => (Some(region), [Some(bank), None]),
                FieldKind::Index { index, data } => (None, [Some(index), Some(data)]),
            };
            if let Some(r) = region
                && let Object::Region(reg) = self.heap.get(self.ns.obj(r)?)?
                && reg.space == Space::PCI_CONFIG.0
                && reg.pci.is_none()
            {
                return Ok(Some(r));
            }
            for m in more.into_iter().flatten() {
                *stack.get_mut(top).ok_or(ErrorKind::Range)? = (m, next);
                top = top.saturating_add(1);
            }
        }
        Ok(None)
    }

    /// For an operator that stores to more than one place (`Divide`): refuses, as an access
    /// would, before anything is stored, if storing to `loc` would need a PCI_Config region's
    /// function not yet found.
    pub(crate) fn target_ready(&mut self, loc: crate::engine::Loc) -> Result<()> {
        use crate::engine::Loc;
        use crate::object::RefTarget;
        let node = match loc {
            Loc::Node(n) | Loc::Ref(RefTarget::Node(n)) => Some(n),
            Loc::Arg(i) => match self.current_call()?.args.get(usize::from(i)) {
                Some(Val::Ref(RefTarget::Node(n))) => Some(*n),
                _ => None,
            },
            _ => None,
        };
        let Some(n) = node else { return Ok(()) };
        let heap = &self.heap;
        let n = self.ns.unalias(n, |o| Self::alias_target(heap, o))?;
        if matches!(self.heap.get(self.ns.obj(n)?)?, Object::FieldUnit(_))
            && let Some(r) = self.unresolved(n)?
        {
            return Err(self.need_address(r));
        }
        Ok(())
    }

    /// Asks for `region`'s function to be found before the access that wanted it: the error the
    /// step that wanted it ends with, put back by the engine.
    pub(crate) fn need_address(&mut self, region: NodeId) -> Error {
        self.pending = Some(region);
        Error::new(ErrorKind::Pending)
    }

    /// Pushes the walk for `region`; refused if one for it is already on the stack.
    pub(crate) fn start_resolve(&mut self, region: NodeId) -> Result<()> {
        if self.resolving > 0 {
            // Each frame looked at is work.
            let n = u64::try_from(self.frames.len()).unwrap_or(u64::MAX);
            self.charge(n / 8)?;
            if self
                .frames
                .iter()
                .any(|f| matches!(f, Frame::Resolve(r) if r.region == region))
            {
                return err(ErrorKind::AddressCycle);
            }
        }
        let scope = self.ns.parent(region).ok_or(ErrorKind::NotFound)?;
        let device = self.device_from(scope).ok_or(ErrorKind::NotFound)?;
        self.push_frame(Frame::Resolve(Resolve {
            region,
            device,
            phase: Phase::Hid,
            at: device,
            asked: device,
            root: None,
            segment: 0,
            bus: 0,
            got: None,
        }))?;
        self.resolving = self.resolving.saturating_add(1);
        Ok(())
    }

    /// Pops a finished walk (`pop_frame` counts it out).
    fn end_resolve(&mut self) {
        self.pop_frame();
    }

    /// Asks `node` for `seg`: absent, a value, or a method started above the frame.
    fn ask(&mut self, node: NodeId, seg: Seg) -> Result<Asked> {
        let Some(c) = self.ns.child(node, seg) else {
            return Ok(Asked::Absent);
        };
        if matches!(self.heap.get(self.ns.obj(c)?)?, Object::Method(_)) {
            self.begin_call(c, [Val::None; 7])?;
            return Ok(Asked::Called);
        }
        self.read_node(c).map(Asked::Value)
    }

    fn is_pci_device(&self, n: NodeId) -> bool {
        self.ns
            .obj(n)
            .ok()
            .and_then(|o| self.heap.get(o).ok())
            .is_some_and(|o| {
                matches!(
                    o,
                    Object::Scope {
                        kind: ScopeKind::Device,
                        ..
                    }
                )
            })
    }

    /// `n` if it is a `Device`, else the nearest `Device` above it; `None` at the root.
    fn device_from(&self, n: NodeId) -> Option<NodeId> {
        let mut n = n;
        loop {
            if self.is_pci_device(n) {
                return Some(n);
            }
            self.ns.note_work(1);
            n = self.ns.parent(n)?;
        }
    }

    /// The next `Device` below `from` on the way down to the device: the device itself last.
    fn next_down(&self, from: NodeId, device: NodeId) -> Result<NodeId> {
        let mut n = device;
        let mut topmost = device;
        loop {
            let p = self.ns.parent(n).ok_or(ErrorKind::NotFound)?;
            self.ns.note_work(1);
            if p == from {
                return Ok(topmost);
            }
            if self.is_pci_device(p) {
                topmost = p;
            }
            n = p;
        }
    }

    /// Whether an ID names a PCI root bridge: an EISA ID or its string, or a package of them
    /// (`_CID`), one level deep.
    fn names_root_bridge(&self, v: Val) -> bool {
        let one = |x: Val| match x {
            Val::Int(i) => ROOT_BRIDGES.contains(&i),
            Val::Obj(r) => match self.heap.get(r) {
                Ok(Object::String(s)) => s.as_slice() == b"PNP0A03" || s.as_slice() == b"PNP0A08",
                Ok(Object::Integer(i)) => ROOT_BRIDGES.contains(i),
                _ => false,
            },
            _ => false,
        };
        match v {
            Val::Obj(r) => match self.heap.get(r) {
                Ok(Object::Package(e)) => {
                    self.note_work(u64::try_from(e.len()).unwrap_or(u64::MAX));
                    e.iter().any(|&x| one(x))
                }
                _ => one(v),
            },
            _ => one(v),
        }
    }

    /// One step of the walk on top of the frames.
    pub(crate) fn resolve_step(&mut self, mut r: Resolve) -> Result<()> {
        // The answer to the question asked, handed back by a method, or asked now.
        let answer = match r.got.take() {
            Some(v) => Some(v),
            None => {
                let seg = match r.phase {
                    Phase::Hid => *b"_HID",
                    Phase::Cid => *b"_CID",
                    Phase::Seg => *b"_SEG",
                    Phase::Bbn => *b"_BBN",
                    Phase::BridgeAdr | Phase::DeviceAdr => *b"_ADR",
                };
                // The frame is written back before a method is started above it.
                self.put_resolve(r)?;
                match self.ask(r.asked, seg)? {
                    Asked::Absent => None,
                    Asked::Value(v) => Some(v),
                    Asked::Called => return Ok(()),
                }
            }
        };
        match r.phase {
            Phase::Hid | Phase::Cid => {
                if answer.is_some_and(|v| self.names_root_bridge(v)) {
                    r.root = Some(r.at);
                    r.phase = Phase::Seg;
                    r.asked = r.at;
                } else if r.phase == Phase::Hid {
                    r.phase = Phase::Cid;
                } else {
                    match self.ns.parent(r.at).and_then(|p| self.device_from(p)) {
                        Some(p) => {
                            r.at = p;
                            r.asked = p;
                            r.phase = Phase::Hid;
                        }
                        // No root bridge above: segment 0, bus 0, no bridges.
                        _ => {
                            r.phase = Phase::DeviceAdr;
                            r.asked = r.device;
                        }
                    }
                }
            }
            Phase::Seg => {
                r.segment = match answer {
                    Some(v) => self.int_of(v)? & 0xFFFF,
                    None => 0,
                };
                r.phase = Phase::Bbn;
            }
            Phase::Bbn => {
                r.bus = match answer {
                    Some(v) => self.int_of(v)? & 0xFF,
                    None => 0,
                };
                let root = r.root.ok_or(ErrorKind::Grammar)?;
                self.after_bridge(&mut r, root)?;
            }
            Phase::BridgeAdr => {
                let bridge = r.asked;
                // A device between with no `_ADR` is not a bridge: passed over.
                let Some(v) = answer else {
                    self.after_bridge(&mut r, bridge)?;
                    return self.put_resolve(r);
                };
                let (device, function) = device_function(self.int_of(v)?)?;
                let here = PciAddress {
                    segment: u16::try_from(r.segment).unwrap_or(0),
                    bus: u8::try_from(r.bus).unwrap_or(0),
                    device,
                    function,
                };
                // Its own request, never a region access with no region (the review of B2,
                // B2-3).
                let request = BridgeBus {
                    bridge: here,
                    time_left_100ns: self.time_for_host()?,
                };
                let got = self.host.bridge_bus(&request);
                self.trace(&Trace::BridgeBus(&request, got));
                r.bus = u64::from(got.map_err(ErrorKind::Host)?);
                self.after_bridge(&mut r, bridge)?;
            }
            Phase::DeviceAdr => {
                // Required: a function the firmware did not name is never taken for function 0.
                let adr = self.int_of(answer.ok_or(ErrorKind::NotFound)?)?;
                let (device, function) = device_function(adr)?;
                let pci = PciAddress {
                    segment: u16::try_from(r.segment).unwrap_or(0),
                    bus: u8::try_from(r.bus).unwrap_or(0),
                    device,
                    function,
                };
                let obj = self.ns.obj(r.region)?;
                if let Object::Region(stored) = self.heap.get_mut(obj)? {
                    stored.pci = Some(pci);
                }
                self.end_resolve();
                return Ok(());
            }
        }
        self.put_resolve(r)
    }

    /// Past `from` (the root bridge, or a bridge): the next bridge's `_ADR`, or the device's.
    fn after_bridge(&mut self, r: &mut Resolve, from: NodeId) -> Result<()> {
        if from == r.device {
            r.phase = Phase::DeviceAdr;
            r.asked = r.device;
            return Ok(());
        }
        r.at = from;
        let next = self.next_down(from, r.device)?;
        if next == r.device {
            r.phase = Phase::DeviceAdr;
            r.asked = r.device;
        } else {
            r.phase = Phase::BridgeAdr;
            r.asked = next;
        }
        Ok(())
    }

    /// Writes the walk back into its frame, on top.
    fn put_resolve(&mut self, r: Resolve) -> Result<()> {
        match self.frames.last_mut() {
            Some(Frame::Resolve(top)) => {
                *top = r;
                Ok(())
            }
            _ => err(ErrorKind::Grammar),
        }
    }

    /// Runs `f`, which may want a PCI_Config region's function first: then the frames find it
    /// and `f` runs again. For the places outside the frames that read fields (an evaluation of a
    /// field unit's own name, and `export`); inside the frames the engine does this itself. The
    /// collector is held off meanwhile, since `f`'s caller holds values outside the frames.
    pub(crate) fn with_address<T>(
        &mut self,
        mut f: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<T> {
        // Each pass finds one region's function for good; a field's chain touches at most a
        // few hundred regions.
        for _ in 0..1_024 {
            match f(self) {
                Err(e) if e.kind == ErrorKind::Pending => {
                    let region = self.pending.take().ok_or(ErrorKind::Grammar)?;
                    let base = self.frames.len();
                    self.start_resolve(region)?;
                    self.nested = self.nested.saturating_add(1);
                    let r = self.run_frames(base);
                    self.nested = self.nested.saturating_sub(1);
                    r?;
                }
                other => return other,
            }
        }
        err(ErrorKind::Range)
    }
}
