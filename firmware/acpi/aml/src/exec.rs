//! The operators (ACPI 6.6 §19.6, by their AML encodings of §20.2.5): what each does once its
//! operands are in.

use alloc::vec::Vec;

use crate::Interpreter;
use crate::convert::Data;
use crate::engine::{Frame, Loc, OpFrame, Operand};
use crate::error::{Error, ErrorKind, Feature, Result, err};
use crate::host::Host;
use crate::mem;
use crate::name::{AmlName, TextName};
use crate::object::{
    BufferField, Connection, FieldKind, FieldUnit, Method, Mutex, NodeId, Object, RefTarget,
    Region, ScopeKind, Val, types,
};
use crate::op::{Kind, Op};
use crate::store::Mode;

fn val(f: &OpFrame, i: usize) -> Result<Val> {
    match f.arg(i) {
        Operand::Val(v) => Ok(v),
        _ => err(ErrorKind::Grammar),
    }
}

fn loc(f: &OpFrame, i: usize) -> Result<Loc> {
    match f.arg(i) {
        Operand::Loc(l) => Ok(l),
        _ => err(ErrorKind::Grammar),
    }
}

fn raw(f: &OpFrame, i: usize) -> Result<u64> {
    match f.arg(i) {
        Operand::Raw(r) => Ok(r),
        _ => err(ErrorKind::Grammar),
    }
}

fn name(f: &OpFrame, i: usize) -> Result<AmlName> {
    match f.arg(i) {
        Operand::Name(n) => Ok(n),
        _ => err(ErrorKind::Grammar),
    }
}

fn low_byte(x: u64) -> u8 {
    x.to_le_bytes().first().copied().unwrap_or(0)
}

/// Whether `kind` is a place.
const fn is_place(kind: Option<Kind>) -> bool {
    matches!(
        kind,
        Some(Kind::Super | Kind::SuperOpt | Kind::Target | Kind::Simple)
    )
}

impl<H: Host> Interpreter<H> {
    fn ones(&self) -> u64 {
        self.width.mask()
    }

    fn truth(&self, b: bool) -> Val {
        Val::Int(if b { self.ones() } else { 0 })
    }

    fn int(&mut self, f: &OpFrame, i: usize) -> Result<u64> {
        let v = val(f, i)?;
        self.int_of(v)
    }

    /// Stores `v` in the target at operand `i`, then hands it on.
    fn result(&mut self, f: &OpFrame, i: usize, v: Val, mode: Mode) -> Result<()> {
        let l = loc(f, i)?;
        self.store(l, v, mode)?;
        self.give_val(v)
    }

    /// An integer result, masked, stored and handed on.
    fn int_result(&mut self, f: &OpFrame, i: usize, x: u64) -> Result<()> {
        let v = Val::Int(x & self.ones());
        self.result(f, i, v, Mode::Store)
    }

    /// Whether the frame below waits for a place rather than a value.
    fn parent_wants_place(&self) -> bool {
        match self.frames.last() {
            Some(Frame::Op(p)) if self.frames.len() > self.base => {
                is_place(p.kinds.get(usize::from(p.n)).copied())
            }
            _ => false,
        }
    }

    /// Runs the operator of the operand frame `f`, whose operands are all in.
    pub(crate) fn complete(&mut self, f: OpFrame) -> Result<()> {
        #[cfg(test)]
        self.note_ran(f.at);
        let bits = u64::from(self.width.bits());
        match f.op {
            // --- Names and named objects -------------------------------------------------------
            Op::Alias => {
                let source = self.resolve(name(&f, 0)?)?.ok_or(ErrorKind::NotFound)?;
                let obj = self.heap.alloc(Object::Alias(source), &mut self.budget)?;
                self.create(name(&f, 1)?, obj)?;
                Ok(())
            }
            Op::Name => {
                // The data object was made fresh by its decoding: it becomes the name's object.
                let obj = match val(&f, 1)? {
                    Val::Int(i) => self.heap.alloc(Object::Integer(i), &mut self.budget)?,
                    Val::Obj(r) => r,
                    _ => return err(ErrorKind::Grammar),
                };
                self.create(name(&f, 0)?, obj)?;
                Ok(())
            }
            Op::Scope => {
                let node = self.resolve(name(&f, 0)?)?.ok_or(ErrorKind::NotFound)?;
                let obj = self.ns.obj(node)?;
                if !matches!(self.heap.get(obj)?, Object::Scope { .. }) {
                    return err(ErrorKind::WrongObject);
                }
                self.open_scope(&f, node)
            }
            Op::Device | Op::Processor | Op::PowerRes | Op::ThermalZone => {
                let kind = match f.op {
                    Op::Device => ScopeKind::Device,
                    Op::Processor => ScopeKind::Processor {
                        id: low_byte(raw(&f, 1)?),
                        pblk: u32::try_from(raw(&f, 2)?).unwrap_or(0),
                        pblk_len: low_byte(raw(&f, 3)?),
                    },
                    Op::PowerRes => ScopeKind::PowerResource {
                        level: low_byte(raw(&f, 1)?),
                        order: u16::try_from(raw(&f, 2)?).unwrap_or(0),
                    },
                    _ => ScopeKind::ThermalZone,
                };
                let obj = self.heap.alloc(Object::Uninitialized, &mut self.budget)?;
                let node = self.create(name(&f, 0)?, obj)?;
                *self.heap.get_mut(obj)? = Object::Scope { kind, node };
                self.open_scope(&f, node)
            }
            Op::Method => {
                let obj = self.heap.alloc(Object::Uninitialized, &mut self.budget)?;
                let node = self.create(name(&f, 0)?, obj)?;
                *self.heap.get_mut(obj)? = Object::Method(Method {
                    table: self.cur.table,
                    start: self.cur.pc,
                    end: f.end,
                    flags: low_byte(raw(&f, 1)?),
                    node,
                });
                self.cur.pc = f.end;
                Ok(())
            }
            // `External` only informs disassemblers (§19.6.46); it creates nothing.
            Op::External => Ok(()),
            Op::Mutex => {
                let sync = low_byte(raw(&f, 1)?) & 0x0F;
                let obj = self.heap.alloc(
                    Object::Mutex(Mutex {
                        sync,
                        depth: 0,
                        global: false,
                    }),
                    &mut self.budget,
                )?;
                self.create(name(&f, 0)?, obj)?;
                Ok(())
            }
            Op::Event => {
                let obj = self.heap.alloc(Object::Event(0), &mut self.budget)?;
                self.create(name(&f, 0)?, obj)?;
                Ok(())
            }
            Op::Region => {
                let space = low_byte(raw(&f, 1)?);
                let offset = self.int(&f, 2)?;
                let length = self.int(&f, 3)?;
                let obj = self.heap.alloc(Object::Uninitialized, &mut self.budget)?;
                let node = self.create(name(&f, 0)?, obj)?;
                *self.heap.get_mut(obj)? = Object::Region(Region {
                    space,
                    offset,
                    length,
                    node,
                    pci: None,
                });
                Ok(())
            }
            Op::DataRegion => self.data_region(&f),
            Op::Field | Op::IndexField | Op::BankField => self.field(&f),
            Op::CreateField
            | Op::CreateBitField
            | Op::CreateByteField
            | Op::CreateWordField
            | Op::CreateDWordField
            | Op::CreateQWordField => self.create_field(&f),

            // --- Data ---------------------------------------------------------------------------
            Op::Buffer => {
                let size = self.int(&f, 0)?;
                let start = usize::try_from(self.cur.pc).map_err(|_| ErrorKind::Truncated)?;
                let end = usize::try_from(f.end).map_err(|_| ErrorKind::Truncated)?;
                let init_len = end.checked_sub(start).ok_or(ErrorKind::PackageLength)?;
                let size =
                    usize::try_from(size).map_err(|_| ErrorKind::Limit(crate::Limit::Memory))?;
                let mut bytes = mem::zeroed(size.max(init_len), &mut self.budget)?;
                let init = self.aml().get(start..end).ok_or(ErrorKind::Truncated)?;
                for (d, s) in bytes.iter_mut().zip(init) {
                    *d = *s;
                }
                self.cur.pc = f.end;
                let v = self.new_buffer(bytes)?;
                self.give_val(v)
            }
            // Packages are built element by element by the engine.
            Op::Package | Op::VarPackage => err(ErrorKind::Grammar),

            // --- References ---------------------------------------------------------------------
            Op::RefOf => {
                let t = self.make_ref(loc(&f, 0)?)?;
                self.give_val(Val::Ref(t))
            }
            Op::CondRefOf => match loc(&f, 0)? {
                Loc::Missing => self.give_val(Val::Int(0)),
                l => {
                    let t = self.make_ref(l)?;
                    self.store(loc(&f, 1)?, Val::Ref(t), Mode::Store)?;
                    let yes = self.truth(true);
                    self.give_val(yes)
                }
            },
            Op::DerefOf => {
                let place = self.parent_wants_place();
                let target = match val(&f, 0)? {
                    Val::Ref(t) => t,
                    v @ Val::Obj(r) if matches!(self.heap.get(r)?, Object::String(_)) => {
                        RefTarget::Node(self.string_path(v)?)
                    }
                    _ => return err(ErrorKind::Type),
                };
                if place {
                    return self.give_val(Val::Ref(target));
                }
                let v = self.read_ref(target)?;
                self.give_val(v)
            }
            Op::Index => {
                let source = val(&f, 0)?;
                let i = self.int(&f, 1)?;
                let index = u32::try_from(i).map_err(|_| ErrorKind::Range)?;
                let r = match source {
                    Val::Obj(r) => r,
                    _ => return err(ErrorKind::Type),
                };
                let len = match self.heap.get(r)? {
                    Object::Package(v) => v.len(),
                    Object::Buffer(b) | Object::String(b) => b.len(),
                    _ => return err(ErrorKind::Type),
                };
                if usize::try_from(index).map_or(true, |k| k >= len) {
                    return err(ErrorKind::Range);
                }
                let t = match self.heap.get(r)? {
                    Object::Package(_) => RefTarget::Elem { pkg: r, index },
                    _ => RefTarget::Byte { obj: r, index },
                };
                self.result(&f, 2, Val::Ref(t), Mode::Store)
            }
            Op::SizeOf => {
                let n = self.size_of(loc(&f, 0)?)?;
                self.give_val(Val::Int(n))
            }
            Op::ObjectType => {
                let t = self.type_of(loc(&f, 0)?)?;
                self.give_val(Val::Int(t))
            }

            // --- Stores -------------------------------------------------------------------------
            Op::Store => {
                let v = val(&f, 0)?;
                let r = self.store(loc(&f, 1)?, v, Mode::Store)?;
                self.give_val(r.unwrap_or(v))
            }
            Op::CopyObject => {
                let v = val(&f, 0)?;
                self.store(loc(&f, 1)?, v, Mode::Copy)?;
                self.give_val(v)
            }
            Op::Increment | Op::Decrement => {
                let l = loc(&f, 0)?;
                let v = self.read_loc(l)?;
                let x = self.int_of(v)?;
                let y = if f.op == Op::Increment {
                    x.wrapping_add(1)
                } else {
                    x.wrapping_sub(1)
                } & self.ones();
                self.store(l, Val::Int(y), Mode::Store)?;
                self.give_val(Val::Int(y))
            }

            // --- Integer arithmetic -------------------------------------------------------------
            Op::Add
            | Op::Subtract
            | Op::Multiply
            | Op::And
            | Op::Nand
            | Op::Or
            | Op::Nor
            | Op::Xor
            | Op::ShiftLeft
            | Op::ShiftRight
            | Op::Mod => {
                let a = self.int(&f, 0)?;
                let b = self.int(&f, 1)?;
                let x = match f.op {
                    Op::Add => a.wrapping_add(b),
                    Op::Subtract => a.wrapping_sub(b),
                    // G9, strict until the reading session answers: the low bits are kept.
                    Op::Multiply => a.wrapping_mul(b),
                    Op::And => a & b,
                    Op::Nand => !(a & b),
                    Op::Or => a | b,
                    Op::Nor => !(a | b),
                    Op::Xor => a ^ b,
                    // G9: a shift by the width or more gives 0.
                    Op::ShiftLeft => {
                        if b >= bits {
                            0
                        } else {
                            a << (b & 63)
                        }
                    }
                    Op::ShiftRight => {
                        if b >= bits {
                            0
                        } else {
                            a >> (b & 63)
                        }
                    }
                    _ => a.checked_rem(b).ok_or(ErrorKind::DivideByZero)?,
                };
                self.int_result(&f, 2, x)
            }
            Op::Divide => {
                let a = self.int(&f, 0)?;
                let b = self.int(&f, 1)?;
                let q = a.checked_div(b).ok_or(ErrorKind::DivideByZero)?;
                let r = a.checked_rem(b).ok_or(ErrorKind::DivideByZero)?;
                // Two stores: both targets' PCI functions are found before the first, so that a
                // step put back for the second never does the first twice (B2-1).
                self.target_ready(loc(&f, 2)?)?;
                self.target_ready(loc(&f, 3)?)?;
                self.store(loc(&f, 2)?, Val::Int(r), Mode::Store)?;
                self.int_result(&f, 3, q)
            }
            Op::Not => {
                let a = self.int(&f, 0)?;
                self.int_result(&f, 1, !a)
            }
            Op::FindSetLeftBit => {
                let a = self.int(&f, 0)?;
                let x = if a == 0 {
                    0
                } else {
                    64u64.saturating_sub(u64::from(a.leading_zeros()))
                };
                self.int_result(&f, 1, x)
            }
            Op::FindSetRightBit => {
                let a = self.int(&f, 0)?;
                let x = if a == 0 {
                    0
                } else {
                    u64::from(a.trailing_zeros()).saturating_add(1)
                };
                self.int_result(&f, 1, x)
            }
            Op::FromBcd => {
                let a = self.int(&f, 0)?;
                let mut x: u64 = 0;
                for shift in (0..16u32).rev().map(|k| k.saturating_mul(4)) {
                    let digit = a.checked_shr(shift).unwrap_or(0) & 0xF;
                    if digit > 9 {
                        return err(ErrorKind::Type);
                    }
                    x = x.saturating_mul(10).saturating_add(digit);
                }
                self.int_result(&f, 1, x)
            }
            Op::ToBcd => {
                let mut a = self.int(&f, 0)?;
                let mut x: u64 = 0;
                let mut shift: u32 = 0;
                while a != 0 {
                    if shift >= self.width.bits() {
                        return err(ErrorKind::Range);
                    }
                    x |= (a % 10).checked_shl(shift).unwrap_or(0);
                    a /= 10;
                    shift = shift.saturating_add(4);
                }
                self.int_result(&f, 1, x)
            }

            // --- Logic --------------------------------------------------------------------------
            Op::LAnd | Op::LOr => {
                let a = self.int(&f, 0)? != 0;
                let b = self.int(&f, 1)? != 0;
                let t = if f.op == Op::LAnd { a && b } else { a || b };
                let v = self.truth(t);
                self.give_val(v)
            }
            Op::LNot => {
                let a = self.int(&f, 0)?;
                let v = self.truth(a == 0);
                self.give_val(v)
            }
            Op::LEqual
            | Op::LNotEqual
            | Op::LGreater
            | Op::LLessEqual
            | Op::LLess
            | Op::LGreaterEqual => {
                let o = self.compare(val(&f, 0)?, val(&f, 1)?)?;
                let t = match f.op {
                    Op::LEqual => o.is_eq(),
                    Op::LNotEqual => o.is_ne(),
                    Op::LGreater => o.is_gt(),
                    Op::LLessEqual => o.is_le(),
                    Op::LLess => o.is_lt(),
                    _ => o.is_ge(),
                };
                let v = self.truth(t);
                self.give_val(v)
            }
            Op::Match => {
                let i = self.match_package(&f)?;
                self.give_val(Val::Int(i))
            }

            // --- Strings and buffers ------------------------------------------------------------
            Op::Concat => {
                let v = self.concatenate(val(&f, 0)?, val(&f, 1)?)?;
                self.result(&f, 2, v, Mode::Store)
            }
            Op::ConcatRes => {
                let v = self.concat_res(val(&f, 0)?, val(&f, 1)?)?;
                self.result(&f, 2, v, Mode::Store)
            }
            Op::Mid => {
                let source = val(&f, 0)?;
                let start = self.int(&f, 1)?;
                let length = self.int(&f, 2)?;
                let string = self.data(source)? == Data::Str;
                let bytes = if string {
                    self.string_of(source)?
                } else {
                    self.buffer_of(source)?
                };
                let from = usize::try_from(start)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let to = from
                    .saturating_add(usize::try_from(length).unwrap_or(usize::MAX))
                    .min(bytes.len());
                let part = mem::copy_bytes(bytes.get(from..to).unwrap_or(&[]), &mut self.budget);
                mem::release(bytes, &mut self.budget);
                let part = part?;
                let v = if string {
                    self.new_string(part)?
                } else {
                    self.new_buffer(part)?
                };
                self.result(&f, 3, v, Mode::Store)
            }
            Op::ToBuffer => {
                let b = self.buffer_of(val(&f, 0)?)?;
                let v = self.new_buffer(b)?;
                self.result(&f, 1, v, Mode::Copy)
            }
            Op::ToDecimalString => {
                let source = val(&f, 0)?;
                let text = match self.data(source)? {
                    Data::Int => {
                        let i = self.int_of(source)?;
                        let mut out = Vec::new();
                        if let Err(e) = self.int_decimal(i, &mut out) {
                            mem::release(out, &mut self.budget);
                            return Err(e);
                        }
                        out
                    }
                    Data::Str => self.string_of(source)?,
                    Data::Buf => {
                        let b = self.buffer_of(source)?;
                        let mut out = Vec::new();
                        let mut r = Ok(());
                        for (k, &byte) in b.iter().enumerate() {
                            if k > 0 {
                                r = mem::extend_bytes(&mut out, b",", &mut self.budget);
                            }
                            if r.is_ok() {
                                r = self.int_decimal(u64::from(byte), &mut out);
                            }
                            if r.is_err() {
                                break;
                            }
                        }
                        mem::release(b, &mut self.budget);
                        if let Err(e) = r {
                            mem::release(out, &mut self.budget);
                            return Err(e);
                        }
                        out
                    }
                    Data::Other(_) => return err(ErrorKind::Type),
                };
                let v = self.new_string(text)?;
                self.result(&f, 1, v, Mode::Copy)
            }
            Op::ToHexString => {
                let source = val(&f, 0)?;
                let text = match self.data(source)? {
                    Data::Int => {
                        let i = self.int_of(source)?;
                        self.int_hex(i)?
                    }
                    Data::Str => self.string_of(source)?,
                    Data::Buf => {
                        let b = self.buffer_of(source)?;
                        let out = self.bytes_hex(&b, b',');
                        mem::release(b, &mut self.budget);
                        out?
                    }
                    Data::Other(_) => return err(ErrorKind::Type),
                };
                let v = self.new_string(text)?;
                self.result(&f, 1, v, Mode::Copy)
            }
            Op::ToInteger => {
                let x = self.explicit_int(val(&f, 0)?)?;
                self.result(&f, 1, Val::Int(x), Mode::Copy)
            }
            Op::ToString => {
                let b = self.buffer_of(val(&f, 0)?)?;
                let limit = self.int(&f, 1)?;
                let max = if limit == self.ones() {
                    usize::MAX
                } else {
                    usize::try_from(limit).unwrap_or(usize::MAX)
                };
                let len = b
                    .iter()
                    .take(max)
                    .position(|&c| c == 0)
                    .unwrap_or_else(|| b.len().min(max));
                let s = mem::copy_bytes(b.get(..len).unwrap_or(&[]), &mut self.budget);
                mem::release(b, &mut self.budget);
                let v = self.new_string(s?)?;
                self.result(&f, 2, v, Mode::Copy)
            }

            // --- Control ------------------------------------------------------------------------
            Op::If => {
                let p = self.int(&f, 0)?;
                self.branch_if(&f, p != 0)
            }
            Op::While => {
                let p = self.int(&f, 0)?;
                self.branch_while(&f, p != 0)
            }
            Op::Break => self.leave_loop(false),
            Op::Continue => self.leave_loop(true),
            Op::Noop => Ok(()),
            Op::Return => {
                if self.calls.is_empty() {
                    return err(ErrorKind::Grammar);
                }
                let v = self.copy_val(val(&f, 0)?)?;
                self.finish_call(v)
            }
            Op::Call => {
                let mut args = [Val::None; 7];
                for (k, a) in args.iter_mut().enumerate().take(usize::from(f.arity)) {
                    *a = val(&f, k)?;
                }
                self.begin_call(f.node, args)
            }

            // --- What reaches the host -------------------------------------------------------------
            Op::Load => err(ErrorKind::NotSupported(Feature::Load)),
            Op::LoadTable => err(ErrorKind::NotSupported(Feature::LoadTable)),
            Op::Stall => {
                let us = self.int(&f, 0)?;
                self.stall(us)
            }
            Op::Sleep => {
                let ms = self.int(&f, 0)?;
                self.sleep(ms)
            }
            Op::Acquire => {
                let m = self.object_at(loc(&f, 0)?)?;
                let v = self.acquire(m, raw(&f, 1)?)?;
                self.give_val(v)
            }
            Op::Release => {
                let m = self.object_at(loc(&f, 0)?)?;
                self.release(m)
            }
            Op::Signal => {
                let e = self.object_at(loc(&f, 0)?)?;
                self.signal(e)
            }
            Op::Reset => {
                let e = self.object_at(loc(&f, 0)?)?;
                self.reset(e)
            }
            Op::Wait => {
                let e = self.object_at(loc(&f, 0)?)?;
                let timeout = self.int(&f, 1)?;
                let v = self.wait(e, timeout)?;
                self.give_val(v)
            }
            Op::Fatal => {
                let kind = low_byte(raw(&f, 0)?);
                let code = u32::try_from(raw(&f, 1)?).unwrap_or(0);
                let arg = self.int(&f, 2)?;
                self.fatal(kind, code, arg)
            }
            Op::Timer => {
                let v = self.timer();
                self.give_val(v)
            }
            Op::Notify => {
                let node = self.node_at(loc(&f, 0)?)?;
                let value = self.int(&f, 1)?;
                self.notify(node, value)
            }
            Op::BreakPoint => {
                self.host.breakpoint();
                Ok(())
            }
        }
    }

    // --- Places --------------------------------------------------------------------------------

    /// A reference to a place (`RefOf`, `CondRefOf`).
    fn make_ref(&self, l: Loc) -> Result<RefTarget> {
        match l {
            Loc::Node(n) => Ok(RefTarget::Node(n)),
            Loc::Ref(t) => Ok(t),
            Loc::Local(slot) => Ok(RefTarget::Local {
                call: self.current_call()?.serial,
                slot,
            }),
            Loc::Arg(slot) => Ok(RefTarget::Arg {
                call: self.current_call()?.serial,
                slot,
            }),
            Loc::Missing => err(ErrorKind::NotFound),
            Loc::Null | Loc::Debug => err(ErrorKind::Type),
        }
    }

    /// The object a place names, as a value: a named object itself (a mutex, an event, a
    /// device), or what a local or argument holds.
    fn object_at(&mut self, l: Loc) -> Result<Val> {
        match l {
            Loc::Node(n) => Ok(Val::Obj(self.ns.obj(n)?)),
            Loc::Ref(RefTarget::Node(n)) => Ok(Val::Obj(self.ns.obj(n)?)),
            other => {
                let v = self.read_loc(other)?;
                match v {
                    Val::Ref(RefTarget::Node(n)) => Ok(Val::Obj(self.ns.obj(n)?)),
                    v => Ok(v),
                }
            }
        }
    }

    /// The node a place names (`Notify`'s operand): a name, or a device passed in a local or an
    /// argument.
    fn node_at(&mut self, l: Loc) -> Result<NodeId> {
        let v = self.object_at(l)?;
        match v {
            Val::Obj(r) => match self.heap.get(r)? {
                Object::Scope { node, .. } => Ok(*node),
                _ => err(ErrorKind::Type),
            },
            _ => err(ErrorKind::Type),
        }
    }

    /// `DataTableRegion` (§19.6.25): a SystemMemory region over the table the XSDT lists under the
    /// signature, OEM ID and OEM table ID.
    fn data_region(&mut self, f: &OpFrame) -> Result<()> {
        let mut key = crate::host::TableKey {
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
            let s = self.string_of(val(f, i.saturating_add(1))?)?;
            let fits = s.len() <= out.len();
            for (d, c) in out.iter_mut().zip(&s) {
                *d = *c;
            }
            mem::release(s, &mut self.budget);
            if !fits {
                return err(ErrorKind::Range);
            }
        }
        let mut found = None;
        let present = self
            .host
            .table(&key, &mut |address, bytes| {
                found = Some((address, u64::try_from(bytes.len()).unwrap_or(0)));
            })
            .map_err(ErrorKind::Host)?;
        let (offset, length) = match found {
            Some(t) if present => t,
            _ => return err(ErrorKind::NotFound),
        };
        let obj = self.heap.alloc(Object::Uninitialized, &mut self.budget)?;
        let node = self.create(name(f, 0)?, obj)?;
        *self.heap.get_mut(obj)? = Object::Region(Region {
            space: 0,
            offset,
            length,
            node,
            pci: None,
        });
        Ok(())
    }

    /// What a place holds, as an operand.
    fn read_loc(&mut self, l: Loc) -> Result<Val> {
        match l {
            Loc::Local(i) => self.local(i),
            Loc::Arg(i) => self.arg(i, false),
            Loc::Node(n) => self.read_node(n),
            Loc::Ref(t) => self.read_ref(t),
            Loc::Missing => err(ErrorKind::NotFound),
            Loc::Null | Loc::Debug => err(ErrorKind::Type),
        }
    }

    /// The value a place holds without reading fields: what `SizeOf` and `ObjectType` look at.
    /// References are followed (a bounded number of times).
    fn peek(&mut self, l: Loc) -> Result<Val> {
        let mut v = match l {
            Loc::Local(i) => self.local(i)?,
            Loc::Arg(i) => self.arg(i, true)?,
            Loc::Node(n) => Val::Obj(self.ns.obj(n)?),
            Loc::Ref(t) => Val::Ref(t),
            Loc::Missing => return err(ErrorKind::NotFound),
            Loc::Null | Loc::Debug => return err(ErrorKind::Type),
        };
        for _ in 0..16 {
            v = match v {
                Val::Ref(RefTarget::Node(n)) => Val::Obj(self.ns.obj(n)?),
                Val::Ref(t @ (RefTarget::Local { .. } | RefTarget::Arg { .. })) => {
                    self.read_ref(t)?
                }
                Val::Ref(RefTarget::Elem { pkg, index }) => match self.heap.get(pkg)? {
                    Object::Package(e) => e
                        .get(usize::try_from(index).map_err(|_| ErrorKind::Range)?)
                        .copied()
                        .ok_or(ErrorKind::Range)?,
                    _ => return err(ErrorKind::Type),
                },
                Val::Obj(r) => match self.heap.get(r)? {
                    Object::Reference(t) => Val::Ref(*t),
                    Object::NameElem(e) => {
                        let e = *e;
                        Val::Obj(self.ns.obj(self.resolve_elem(e)?)?)
                    }
                    _ => return Ok(v),
                },
                other => return Ok(other),
            };
        }
        err(ErrorKind::Type)
    }

    fn size_of(&mut self, l: Loc) -> Result<u64> {
        let v = self.peek(l)?;
        let n = match v {
            Val::Obj(r) => match self.heap.get(r)? {
                Object::Buffer(b) | Object::String(b) => b.len(),
                Object::Package(p) => p.len(),
                _ => return err(ErrorKind::Type),
            },
            Val::None => return err(ErrorKind::Uninitialized),
            _ => return err(ErrorKind::Type),
        };
        u64::try_from(n).map_err(|_| Error::from(ErrorKind::Range))
    }

    fn type_of(&mut self, l: Loc) -> Result<u64> {
        if l == Loc::Debug {
            return Ok(types::DEBUG);
        }
        let v = self.peek(l)?;
        Ok(match v {
            Val::None => types::UNINITIALIZED,
            Val::Int(_) => types::INTEGER,
            Val::Ref(RefTarget::Byte { .. }) => types::BUFFER_FIELD,
            Val::Ref(_) => types::UNINITIALIZED,
            Val::Obj(r) => self.heap.get(r)?.type_code(),
        })
    }

    /// The node a string names, relative to the current scope (`DerefOf` of a string).
    fn string_path(&self, v: Val) -> Result<NodeId> {
        let Val::Obj(r) = v else {
            return err(ErrorKind::Type);
        };
        let Object::String(s) = self.heap.get(r)? else {
            return err(ErrorKind::Type);
        };
        let text = core::str::from_utf8(s).map_err(|_| ErrorKind::Name)?;
        let path = TextName::parse(text).map_err(|_| ErrorKind::Name)?;
        let heap = &self.heap;
        let n = self
            .ns
            .lookup(self.scope(), &path, |o| Self::alias_target(heap, o))?
            .ok_or(ErrorKind::NotFound)?;
        self.ns.unalias(n, |o| Self::alias_target(heap, o))
    }

    // --- Operators with more to them ---------------------------------------------------------

    /// `Concatenate` (Tables 19.30 and 19.31).
    fn concatenate(&mut self, a: Val, b: Val) -> Result<Val> {
        let kind = self.data(a)?;
        let (first, string) = match kind {
            Data::Int => {
                let x = self.int_of(a)?;
                (self.int_bytes(x)?, false)
            }
            Data::Buf => (self.buffer_of(a)?, false),
            Data::Str => (self.string_of(a)?, true),
            Data::Other(t) => (
                mem::copy_bytes(crate::object::Object::type_text(t), &mut self.budget)?,
                true,
            ),
        };
        let second = match (kind, self.data(b)) {
            (Data::Int, _) => {
                let y = self.int_of(b);
                match y {
                    Ok(y) => self.int_bytes(y),
                    Err(e) => Err(e),
                }
            }
            (Data::Buf, Ok(Data::Other(t))) => {
                // Other types concatenate as the string naming them, as a buffer.
                mem::copy_bytes(Object::type_text(t), &mut self.budget)
            }
            (Data::Buf, _) => self.buffer_of(b),
            (_, Ok(Data::Other(t))) => mem::copy_bytes(Object::type_text(t), &mut self.budget),
            _ => self.string_of(b),
        };
        let mut out = first;
        match second {
            Ok(s) => {
                let r = mem::extend_bytes(&mut out, &s, &mut self.budget);
                mem::release(s, &mut self.budget);
                if let Err(e) = r {
                    mem::release(out, &mut self.budget);
                    return Err(e);
                }
            }
            Err(e) => {
                mem::release(out, &mut self.budget);
                return Err(e);
            }
        }
        if string {
            self.new_string(out)
        } else {
            self.new_buffer(out)
        }
    }

    /// The descriptors of a resource template, without its end tag (§6.4).
    fn descriptors(bytes: &[u8]) -> Result<&[u8]> {
        if bytes.is_empty() {
            return Ok(&[]);
        }
        if bytes.len() == 1 {
            return err(ErrorKind::Range);
        }
        let mut at: usize = 0;
        while let Some(&tag) = bytes.get(at) {
            if tag & 0x80 == 0 {
                if tag >> 3 == 0x0F {
                    return bytes.get(..at).ok_or_else(|| Error::from(ErrorKind::Range));
                }
                at = at.saturating_add(1).saturating_add(usize::from(tag & 7));
            } else {
                let lo = bytes
                    .get(at.saturating_add(1))
                    .copied()
                    .ok_or(ErrorKind::Range)?;
                let hi = bytes
                    .get(at.saturating_add(2))
                    .copied()
                    .ok_or(ErrorKind::Range)?;
                let len = usize::from(u16::from_le_bytes([lo, hi]));
                at = at.saturating_add(3).saturating_add(len);
            }
        }
        err(ErrorKind::Range)
    }

    /// `ConcatenateResTemplate` (§19.6.13): both lists of descriptors, a new end tag and a
    /// checksum making the whole sum to zero.
    fn concat_res(&mut self, a: Val, b: Val) -> Result<Val> {
        let x = self.buffer_of(a)?;
        let y = match self.buffer_of(b) {
            Ok(y) => y,
            Err(e) => {
                mem::release(x, &mut self.budget);
                return Err(e);
            }
        };
        let joined = (|| -> Result<Vec<u8>> {
            let dx = Self::descriptors(&x)?;
            let dy = Self::descriptors(&y)?;
            let mut out = mem::with_capacity(
                dx.len().saturating_add(dy.len()).saturating_add(2),
                &mut self.budget,
            )?;
            mem::extend_bytes(&mut out, dx, &mut self.budget)?;
            mem::extend_bytes(&mut out, dy, &mut self.budget)?;
            mem::extend_bytes(&mut out, &[0x79], &mut self.budget)?;
            let sum = out.iter().fold(0u8, |s, &c| s.wrapping_add(c));
            mem::extend_bytes(&mut out, &[0u8.wrapping_sub(sum)], &mut self.budget)?;
            Ok(out)
        })();
        mem::release(x, &mut self.budget);
        mem::release(y, &mut self.budget);
        let out = joined?;
        self.new_buffer(out)
    }

    /// `Match` (§19.6.81): the first index from the start whose element satisfies both
    /// comparisons; elements that are not integers, strings or buffers, or will not convert, never
    /// match. Only such failures are "does not match": a limit, a host's refusal or anything else
    /// ends the evaluation (the review of B2's Addendum 2, N2-2).
    fn match_package(&mut self, f: &OpFrame) -> Result<u64> {
        /// A failure that only means the element does not convert, and so never matches.
        fn no_match(e: &Error) -> bool {
            matches!(
                e.kind,
                ErrorKind::Type | ErrorKind::Uninitialized | ErrorKind::NotFound
            )
        }
        let Val::Obj(pkg) = val(f, 0)? else {
            return err(ErrorKind::Type);
        };
        let n = match self.heap.get(pkg)? {
            Object::Package(e) => e.len(),
            _ => return err(ErrorKind::Type),
        };
        let ops = [raw(f, 1)?, raw(f, 3)?];
        let objs = [val(f, 2)?, val(f, 4)?];
        let start = self.int(f, 5)?;
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        // Before any element is read: a field among them whose PCI function is not yet known
        // asks for it now, so that the step put back for it has read nothing (B2-1).
        for k in start..n {
            self.charge(1)?;
            let e = match self.heap.get(pkg)? {
                Object::Package(v) => v.get(k).copied().unwrap_or(Val::None),
                _ => Val::None,
            };
            if let Some(region) = self.unresolved_element(e)? {
                return Err(self.need_address(region));
            }
        }
        for k in start..n {
            self.charge(1)?;
            let e = match self.heap.get(pkg)? {
                Object::Package(v) => v.get(k).copied().unwrap_or(Val::None),
                _ => Val::None,
            };
            let e = match self.element(e) {
                Ok(e) => e,
                Err(e) if no_match(&e) => continue,
                Err(e) => return Err(e),
            };
            if !matches!(self.data(e), Ok(Data::Int | Data::Str | Data::Buf)) {
                continue;
            }
            let mut all = true;
            for (op, obj) in ops.iter().zip(objs) {
                let ok = match op {
                    0 => true,
                    1..=5 => match self.compare_as(obj, e) {
                        Ok(o) => match op {
                            1 => o.is_eq(),
                            2 => o.is_le(),
                            3 => o.is_lt(),
                            4 => o.is_ge(),
                            _ => o.is_gt(),
                        },
                        Err(e) if no_match(&e) => false,
                        Err(e) => return Err(e),
                    },
                    _ => return err(ErrorKind::Type),
                };
                all &= ok;
            }
            if all {
                return u64::try_from(k).map_err(|_| Error::from(ErrorKind::Range));
            }
        }
        Ok(self.ones())
    }

    /// The order of the element `e` against `obj`, `e` converted to `obj`'s type.
    fn compare_as(&mut self, obj: Val, e: Val) -> Result<core::cmp::Ordering> {
        Ok(self.compare(obj, e)?.reverse())
    }

    // --- Buffer fields -------------------------------------------------------------------------

    fn create_field(&mut self, f: &OpFrame) -> Result<()> {
        let source = val(f, 0)?;
        let index = self.int(f, 1)?;
        let (bit_offset, bit_len, name_at) = match f.op {
            Op::CreateField => (index, self.int(f, 2)?, 3),
            Op::CreateBitField => (index, 1, 2),
            Op::CreateByteField => (index.checked_mul(8).ok_or(ErrorKind::Range)?, 8, 2),
            Op::CreateWordField => (index.checked_mul(8).ok_or(ErrorKind::Range)?, 16, 2),
            Op::CreateDWordField => (index.checked_mul(8).ok_or(ErrorKind::Range)?, 32, 2),
            _ => (index.checked_mul(8).ok_or(ErrorKind::Range)?, 64, 2),
        };
        if bit_len == 0 {
            return err(ErrorKind::Range);
        }
        let buffer = match source {
            Val::Obj(r) if matches!(self.heap.get(r)?, Object::Buffer(_)) => r,
            other => {
                let b = self.buffer_of(other)?;
                match self.new_buffer(b)? {
                    Val::Obj(r) => r,
                    _ => return err(ErrorKind::Type),
                }
            }
        };
        let len = match self.heap.get(buffer)? {
            Object::Buffer(b) => b.len(),
            _ => return err(ErrorKind::Type),
        };
        let have = u64::try_from(len)
            .ok()
            .and_then(|n| n.checked_mul(8))
            .ok_or(ErrorKind::Range)?;
        #[cfg(test)]
        let have = if crate::tests::planted(crate::tests::Fault::WeakFieldBound) {
            have.saturating_add(8)
        } else {
            have
        };
        if bit_offset.checked_add(bit_len).is_none_or(|end| end > have) {
            return err(ErrorKind::Range);
        }
        let obj = self.heap.alloc(
            Object::BufferField(BufferField {
                source: buffer,
                bit_offset,
                bit_len,
            }),
            &mut self.budget,
        )?;
        self.create(name(f, name_at)?, obj)?;
        Ok(())
    }

    // --- Field lists (§20.2.5.2) ---------------------------------------------------------------

    fn field(&mut self, f: &OpFrame) -> Result<()> {
        let flags_at = match f.op {
            Op::Field => 1,
            Op::IndexField => 2,
            _ => 3,
        };
        let flags = low_byte(raw(f, flags_at)?);
        if flags & 0x80 != 0 {
            return err(ErrorKind::Grammar);
        }
        let kind = match f.op {
            Op::Field => {
                let region = self.resolve(name(f, 0)?)?.ok_or(ErrorKind::NotFound)?;
                self.expect(region, |o| matches!(o, Object::Region(_)))?;
                FieldKind::Region(region)
            }
            Op::IndexField => {
                let index = self.resolve(name(f, 0)?)?.ok_or(ErrorKind::NotFound)?;
                let data = self.resolve(name(f, 1)?)?.ok_or(ErrorKind::NotFound)?;
                self.expect(index, |o| matches!(o, Object::FieldUnit(_)))?;
                self.expect(data, |o| matches!(o, Object::FieldUnit(_)))?;
                FieldKind::Index { index, data }
            }
            _ => {
                let region = self.resolve(name(f, 0)?)?.ok_or(ErrorKind::NotFound)?;
                let bank = self.resolve(name(f, 1)?)?.ok_or(ErrorKind::NotFound)?;
                self.expect(region, |o| matches!(o, Object::Region(_)))?;
                self.expect(bank, |o| matches!(o, Object::FieldUnit(_)))?;
                let value = self.int(f, 2)?;
                FieldKind::Bank {
                    region,
                    bank,
                    value,
                }
            }
        };
        let mut unit = FieldUnit {
            kind,
            bit_offset: 0,
            bit_len: 0,
            access: flags & 0x0F,
            attrib: 0,
            access_len: 0,
            lock: flags & 0x10 != 0,
            update: (flags >> 5) & 3,
            connection: Connection::None,
            conn_bit_offset: 0,
        };
        // Where the last `Connection` was, in bits from the region's start.
        let mut conn_at: u64 = 0;
        while self.cur.pc < f.end {
            // Each entry is work, and so is finding its name among its siblings: a list of
            // hundreds of thousands of names is cut off by the limits inside this one step.
            self.charge(1)?;
            self.charge_pending()?;
            let at = self.cur.pc;
            let next = at.checked_add(1).ok_or(ErrorKind::Truncated)?;
            match self.byte(at)? {
                0x00 => {
                    let (bits, after) = self.pkg_length(next)?;
                    unit.bit_offset = unit
                        .bit_offset
                        .checked_add(u64::from(bits))
                        .ok_or(ErrorKind::Range)?;
                    self.cur.pc = after;
                }
                0x01 => {
                    let access = self.byte(next)?;
                    let attrib = self.byte(next.checked_add(1).ok_or(ErrorKind::Truncated)?)?;
                    unit.access = access & 0x0F;
                    match access >> 6 {
                        0 => {
                            unit.attrib = attrib;
                            unit.access_len = 0;
                        }
                        k => {
                            unit.attrib = match k {
                                1 => 0x0B,
                                2 => 0x0E,
                                _ => 0x0F,
                            };
                            unit.access_len = attrib;
                        }
                    }
                    self.cur.pc = next.checked_add(2).ok_or(ErrorKind::Truncated)?;
                }
                0x03 => {
                    let access = self.byte(next)?;
                    let attrib = self.byte(next.checked_add(1).ok_or(ErrorKind::Truncated)?)?;
                    let length = self.byte(next.checked_add(2).ok_or(ErrorKind::Truncated)?)?;
                    unit.access = access & 0x0F;
                    unit.attrib = attrib;
                    unit.access_len = length;
                    self.cur.pc = next.checked_add(3).ok_or(ErrorKind::Truncated)?;
                }
                0x02 => {
                    self.cur.pc = next;
                    unit.connection = self.connection(f.end)?;
                    conn_at = unit.bit_offset;
                }
                _ => {
                    let (bits, after) =
                        self.pkg_length(at.checked_add(4).ok_or(ErrorKind::Truncated)?)?;
                    unit.bit_len = u64::from(bits);
                    unit.conn_bit_offset = unit.bit_offset.saturating_sub(conn_at);
                    let obj = self.heap.alloc(Object::FieldUnit(unit), &mut self.budget)?;
                    let field_name = AmlName {
                        root: false,
                        parents: 0,
                        first: at,
                        count: 1,
                    };
                    self.create(field_name, obj)?;
                    unit.bit_offset = unit
                        .bit_offset
                        .checked_add(u64::from(bits))
                        .ok_or(ErrorKind::Range)?;
                    self.cur.pc = after;
                }
            }
        }
        if self.cur.pc != f.end {
            return err(ErrorKind::PackageLength);
        }
        Ok(())
    }

    /// A field list's `Connection`: a name, or a buffer whose size is a constant.
    fn connection(&mut self, end: u32) -> Result<Connection> {
        let at = self.cur.pc;
        let b = self.byte(at)?;
        if crate::name::starts_name(b) {
            let (_, after) = crate::name::parse(self.aml(), at)?;
            self.cur.pc = after;
            return Ok(Connection::Name(crate::object::NameElem {
                scope: self.scope(),
                table: self.cur.table,
                at,
            }));
        }
        if b != 0x11 {
            return err(ErrorKind::Grammar);
        }
        let op_end = at.checked_add(1).ok_or(ErrorKind::Truncated)?;
        let (len, body) = self.pkg_length(op_end)?;
        let buf_end = op_end.checked_add(len).ok_or(ErrorKind::Truncated)?;
        if buf_end > end {
            return err(ErrorKind::PackageLength);
        }
        let (size, data) = match self.byte(body)? {
            0x00 => (0, body.checked_add(1)),
            0x01 => (1, body.checked_add(1)),
            0x0A => (
                u64::from(self.byte(body.checked_add(1).ok_or(ErrorKind::Truncated)?)?),
                body.checked_add(2),
            ),
            0x0B => {
                let lo = self.byte(body.checked_add(1).ok_or(ErrorKind::Truncated)?)?;
                let hi = self.byte(body.checked_add(2).ok_or(ErrorKind::Truncated)?)?;
                (u64::from(u16::from_le_bytes([lo, hi])), body.checked_add(3))
            }
            _ => return err(ErrorKind::Grammar),
        };
        let data = data.ok_or(ErrorKind::Truncated)?;
        let from = usize::try_from(data).map_err(|_| ErrorKind::Truncated)?;
        let to = usize::try_from(buf_end).map_err(|_| ErrorKind::Truncated)?;
        let init = self.aml().get(from..to).ok_or(ErrorKind::PackageLength)?;
        let size = usize::try_from(size).unwrap_or(0).max(init.len());
        let mut bytes = mem::zeroed(size, &mut self.budget)?;
        let init = self.aml().get(from..to).unwrap_or(&[]);
        for (d, s) in bytes.iter_mut().zip(init) {
            *d = *s;
        }
        self.cur.pc = buf_end;
        let r = self.heap.alloc(Object::Buffer(bytes), &mut self.budget)?;
        Ok(Connection::Buffer(r))
    }

    /// Checks the object of `node`.
    fn expect(&self, node: NodeId, ok: impl Fn(&Object) -> bool) -> Result<()> {
        let obj = self.ns.obj(node)?;
        if ok(self.heap.get(obj)?) {
            Ok(())
        } else {
            err(ErrorKind::WrongObject)
        }
    }
}
