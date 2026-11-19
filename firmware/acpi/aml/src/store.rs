//! Storing and copying (ACPI 6.6 §19.3.5.5 and §19.3.5.8, Tables 19.8 to 19.14), and buffer
//! fields.
//!
//! `Store` and every operator's `Target` convert to a named object's own type; `CopyObject` and the
//! explicit conversion operators replace it. A local takes a copy as it is. An argument holding a
//! reference stores through it. A field keeps its type. Copies of packages are deep, made with a
//! work list rather than recursion.

use alloc::vec::Vec;

use crate::Interpreter;
use crate::engine::Loc;
use crate::error::{Error, ErrorKind, Result, err};
use crate::host::Host;
use crate::mem;
use crate::object::{BufferField, NodeId, ObjRef, Object, RefTarget, Val};

/// How a store treats a named target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// `Store` and `Target`s: convert to the target's type.
    Store,
    /// `CopyObject` and the explicit conversion operators: replace object and type.
    Copy,
}

fn idx(i: u32) -> Result<usize> {
    usize::try_from(i).map_err(|_| Error::from(ErrorKind::Range))
}

/// Reads `len` bits of `src` from bit `off` into `out` (zeroed, at least `ceil(len / 8)` bytes).
pub(crate) fn get_bits(src: &[u8], off: u64, len: u64, out: &mut [u8]) -> Result<()> {
    let end = off.checked_add(len).ok_or(ErrorKind::Range)?;
    let have = u64::try_from(src.len())
        .ok()
        .and_then(|n| n.checked_mul(8))
        .ok_or(ErrorKind::Range)?;
    if end > have {
        return err(ErrorKind::Range);
    }
    let first = usize::try_from(off / 8).map_err(|_| ErrorKind::Range)?;
    let shift = u32::try_from(off % 8).unwrap_or(0);
    let bytes = usize::try_from(len.div_ceil(8)).map_err(|_| ErrorKind::Range)?;
    for (k, o) in out.iter_mut().take(bytes).enumerate() {
        let at = first.saturating_add(k);
        let lo = u16::from(src.get(at).copied().unwrap_or(0));
        let hi = u16::from(src.get(at.saturating_add(1)).copied().unwrap_or(0));
        let word = (lo | (hi << 8)) >> shift;
        *o = word.to_le_bytes().first().copied().unwrap_or(0);
    }
    let tail = u32::try_from(len % 8).unwrap_or(0);
    if tail != 0
        && let Some(last) = out.get_mut(bytes.saturating_sub(1))
    {
        *last &= 0xFFu8
            .checked_shr(8u32.saturating_sub(tail))
            .unwrap_or(0xFF);
    }
    Ok(())
}

/// Writes `len` bits at bit `off` of `dst` from `src`, which is zero-extended or truncated.
pub(crate) fn set_bits(dst: &mut [u8], off: u64, len: u64, src: &[u8]) -> Result<()> {
    let end = off.checked_add(len).ok_or(ErrorKind::Range)?;
    let have = u64::try_from(dst.len())
        .ok()
        .and_then(|n| n.checked_mul(8))
        .ok_or(ErrorKind::Range)?;
    if end > have {
        return err(ErrorKind::Range);
    }
    let mut done: u64 = 0;
    while done < len {
        let n = u32::try_from(len.saturating_sub(done).min(8)).unwrap_or(8);
        let byte_i = usize::try_from(done / 8).map_err(|_| ErrorKind::Range)?;
        let value = u16::from(src.get(byte_i).copied().unwrap_or(0));
        let mask = 0xFFu16.checked_shr(8u32.saturating_sub(n)).unwrap_or(0xFF);
        let bit = off.saturating_add(done);
        let at = usize::try_from(bit / 8).map_err(|_| ErrorKind::Range)?;
        let shift = u32::try_from(bit % 8).unwrap_or(0);
        let wide_mask = mask << shift;
        let wide = (value & mask) << shift;
        let [mlo, mhi] = wide_mask.to_le_bytes();
        let [vlo, vhi] = wide.to_le_bytes();
        if let Some(b) = dst.get_mut(at) {
            *b = (*b & !mlo) | vlo;
        }
        if mhi != 0
            && let Some(b) = dst.get_mut(at.saturating_add(1))
        {
            *b = (*b & !mhi) | vhi;
        }
        done = done.saturating_add(u64::from(n));
    }
    Ok(())
}

impl<H: Host> Interpreter<H> {
    /// A copy of a data object, as an object not yet in the heap. Packages are copied deeply.
    pub(crate) fn copy_object(&mut self, v: Val) -> Result<Object> {
        match v {
            Val::Int(i) => Ok(Object::Integer(i & self.width.mask())),
            Val::Ref(t) => Ok(Object::Reference(t)),
            Val::None => err(ErrorKind::Uninitialized),
            Val::Obj(r) => {
                let pkg = match self.heap.get(r)? {
                    Object::Integer(i) => return Ok(Object::Integer(*i)),
                    Object::String(s) => {
                        return Ok(Object::String(mem::copy_bytes(s, &mut self.budget)?));
                    }
                    Object::Buffer(b) => {
                        return Ok(Object::Buffer(mem::copy_bytes(b, &mut self.budget)?));
                    }
                    Object::Package(_) => r,
                    _ => return err(ErrorKind::Type),
                };
                let elems = self.copy_package(pkg)?;
                Ok(Object::Package(elems))
            }
        }
    }

    /// The elements of a deep copy of the package `src`.
    fn copy_package(&mut self, src: ObjRef) -> Result<Vec<Val>> {
        let top = self.shallow_elements(src)?;
        // Nested packages: each placeholder made by `shallow_elements` is filled from its source.
        let mut work: Vec<(ObjRef, ObjRef)> = Vec::new();
        let result = (|| {
            for &e in &top {
                self.queue_nested(e, &mut work)?;
            }
            while let Some((from, to)) = work.pop() {
                let elems = self.shallow_elements(from)?;
                let mut queued = Ok(());
                for &e in &elems {
                    queued = self.queue_nested(e, &mut work);
                    if queued.is_err() {
                        break;
                    }
                }
                if let Err(e) = queued {
                    mem::release(elems, &mut self.budget);
                    return Err(e);
                }
                self.heap
                    .replace(to, Object::Package(elems), &mut self.budget)?;
            }
            Ok(())
        })();
        mem::release(work, &mut self.budget);
        match result {
            Ok(()) => Ok(top),
            Err(e) => {
                mem::release(top, &mut self.budget);
                Err(e)
            }
        }
    }

    /// Element copies of `src`: strings and buffers copied, packages as empty placeholders whose
    /// source is remembered in a `Reference` until they are filled, anything else shared.
    fn shallow_elements(&mut self, src: ObjRef) -> Result<Vec<Val>> {
        let n = match self.heap.get(src)? {
            Object::Package(v) => v.len(),
            _ => return err(ErrorKind::Type),
        };
        let mut out = mem::with_capacity(n, &mut self.budget)?;
        for k in 0..n {
            match self.shallow_element(src, k) {
                // `out` was made with room for every element: this push cannot fail.
                Ok(copied) => mem::push(&mut out, copied, &mut self.budget)?,
                Err(e) => {
                    // What was copied so far is unreachable, and the collector takes it; the
                    // vector's bytes are credited here (the review of B1, F2).
                    mem::release(out, &mut self.budget);
                    return Err(e);
                }
            }
        }
        Ok(out)
    }

    /// A copy of element `k` of the package `src`, as `shallow_elements` makes it.
    fn shallow_element(&mut self, src: ObjRef, k: usize) -> Result<Val> {
        let e = match self.heap.get(src)? {
            Object::Package(v) => v.get(k).copied().unwrap_or(Val::None),
            _ => Val::None,
        };
        Ok(match e {
            Val::Obj(r) => match self.heap.get(r)? {
                Object::String(_) | Object::Buffer(_) | Object::Integer(_) => {
                    let o = self.copy_object(e)?;
                    Val::Obj(self.heap.alloc(o, &mut self.budget)?)
                }
                Object::Package(_) => {
                    // A placeholder that records its source; `copy_package` fills it.
                    Val::Obj(self.heap.alloc(Object::Placeholder(r), &mut self.budget)?)
                }
                _ => e,
            },
            other => other,
        })
    }

    fn queue_nested(&mut self, e: Val, work: &mut Vec<(ObjRef, ObjRef)>) -> Result<()> {
        if let Val::Obj(to) = e
            && let Object::Placeholder(from) = *self.heap.get(to)?
        {
            mem::push(work, (from, to), &mut self.budget)?;
        }
        Ok(())
    }

    /// A copy of `v` for a local, an argument, an element or a method's result: data copied,
    /// integers and references as they are, other objects shared.
    pub(crate) fn copy_val(&mut self, v: Val) -> Result<Val> {
        match v {
            Val::Obj(r) => match self.heap.get(r)? {
                Object::String(_) | Object::Buffer(_) | Object::Package(_) | Object::Integer(_) => {
                    let o = self.copy_object(v)?;
                    Ok(Val::Obj(self.heap.alloc(o, &mut self.budget)?))
                }
                _ => Ok(v),
            },
            other => Ok(other),
        }
    }

    /// Stores `v` in `loc`. `Some` is what `Store` gives instead of `v`: the buffer a
    /// buffer-protocol field's host returned (§19.6.132).
    pub(crate) fn store(&mut self, loc: Loc, v: Val, mode: Mode) -> Result<Option<Val>> {
        match loc {
            Loc::Null => Ok(None),
            Loc::Debug => self.debug(v).map(|()| None),
            Loc::Missing => err(ErrorKind::NotFound),
            Loc::Local(i) => {
                let c = self.copy_val(v)?;
                let call = self.calls.last_mut().ok_or(ErrorKind::Grammar)?;
                *call
                    .locals
                    .get_mut(usize::from(i))
                    .ok_or(ErrorKind::Grammar)? = c;
                Ok(None)
            }
            Loc::Arg(i) => {
                let cur = self.current_call()?.args.get(usize::from(i)).copied();
                match cur {
                    Some(Val::Ref(t)) => self.store_ref(t, v, mode),
                    Some(_) => {
                        let c = self.copy_val(v)?;
                        let call = self.calls.last_mut().ok_or(ErrorKind::Grammar)?;
                        *call
                            .args
                            .get_mut(usize::from(i))
                            .ok_or(ErrorKind::Grammar)? = c;
                        Ok(None)
                    }
                    None => err(ErrorKind::Grammar),
                }
            }
            Loc::Node(n) => self.store_node(n, v, mode),
            Loc::Ref(t) => self.store_ref(t, v, mode),
        }
    }

    fn store_ref(&mut self, t: RefTarget, v: Val, mode: Mode) -> Result<Option<Val>> {
        match t {
            RefTarget::Node(n) => self.store_node(n, v, mode),
            RefTarget::Local { call, slot } => {
                let c = self.copy_val(v)?;
                *self
                    .call_frame_mut(call)?
                    .locals
                    .get_mut(usize::from(slot))
                    .ok_or(ErrorKind::Grammar)? = c;
                Ok(None)
            }
            RefTarget::Arg { call, slot } => {
                let c = self.copy_val(v)?;
                *self
                    .call_frame_mut(call)?
                    .args
                    .get_mut(usize::from(slot))
                    .ok_or(ErrorKind::Grammar)? = c;
                Ok(None)
            }
            RefTarget::Elem { pkg, index } => {
                let c = self.copy_val(v)?;
                match self.heap.get_mut(pkg)? {
                    Object::Package(elems) => {
                        *elems.get_mut(idx(index)?).ok_or(ErrorKind::Range)? = c;
                        Ok(None)
                    }
                    _ => err(ErrorKind::Type),
                }
            }
            RefTarget::Byte { obj, index } => {
                // An 8-bit buffer field (§19.6.63.2): an integer's low byte, a buffer's or a
                // string's first.
                let byte = match v {
                    Val::Int(i) => i.to_le_bytes().first().copied().unwrap_or(0),
                    Val::Obj(r) => match self.heap.get(r)? {
                        Object::Integer(i) => i.to_le_bytes().first().copied().unwrap_or(0),
                        Object::Buffer(b) | Object::String(b) => b.first().copied().unwrap_or(0),
                        _ => return err(ErrorKind::Type),
                    },
                    Val::None => return err(ErrorKind::Uninitialized),
                    Val::Ref(_) => return err(ErrorKind::Type),
                };
                match self.heap.get_mut(obj)? {
                    Object::Buffer(b) | Object::String(b) => {
                        *b.get_mut(idx(index)?).ok_or(ErrorKind::Range)? = byte;
                        Ok(None)
                    }
                    _ => err(ErrorKind::Type),
                }
            }
        }
    }

    /// Stores `v` in the named object `n` (Table 19.14).
    pub(crate) fn store_node(&mut self, n: NodeId, v: Val, mode: Mode) -> Result<Option<Val>> {
        let obj = self.ns.obj(n)?;
        enum Target {
            Int,
            Str,
            Buf,
            Pkg,
            Field(BufferField),
            Region,
            Replace,
        }
        let target = match self.heap.get(obj)? {
            Object::Integer(_) => Target::Int,
            Object::String(_) => Target::Str,
            Object::Buffer(_) => Target::Buf,
            Object::Package(_) => Target::Pkg,
            Object::BufferField(f) => Target::Field(*f),
            Object::FieldUnit(_) => Target::Region,
            Object::Reference(_) | Object::Uninitialized => Target::Replace,
            _ => return err(ErrorKind::Type),
        };
        match (mode, target) {
            (Mode::Copy, Target::Region) => match self.data(v)? {
                crate::convert::Data::Int | crate::convert::Data::Buf => self.write_field(n, v),
                _ => err(ErrorKind::Type),
            },
            (Mode::Store, Target::Region) => self.write_field(n, v),
            (Mode::Copy, Target::Field(f)) => {
                // `CopyObject` to a field takes an integer or a buffer only (Table 19.8).
                match self.data(v)? {
                    crate::convert::Data::Int | crate::convert::Data::Buf => {
                        self.write_buffer_field(&f, v).map(|()| None)
                    }
                    _ => err(ErrorKind::Type),
                }
            }
            (Mode::Store, Target::Field(f)) => self.write_buffer_field(&f, v).map(|()| None),
            // `CopyObject` gives the name a new object (the review of B1, F13): whatever held the
            // old one (a buffer field over it, an `Index` reference, a caller's argument) keeps
            // it, and the collector frees it when nothing does.
            (Mode::Copy, _) => {
                let o = self.copy_object(v)?;
                let new = self.heap.alloc(o, &mut self.budget)?;
                self.ns.set_obj(n, new).map(|()| None)
            }
            (Mode::Store, Target::Replace) => {
                let o = self.copy_object(v)?;
                self.heap.replace(obj, o, &mut self.budget).map(|()| None)
            }
            (Mode::Store, Target::Int) => {
                let i = self.int_of(v)?;
                *self.heap.get_mut(obj)? = Object::Integer(i);
                Ok(None)
            }
            (Mode::Store, Target::Str) => {
                let s = self.string_of(v)?;
                self.heap
                    .replace(obj, Object::String(s), &mut self.budget)
                    .map(|()| None)
            }
            (Mode::Store, Target::Buf) => {
                // G11 (TASKS 64's ledger §3): a named buffer keeps its length, except that a
                // zero-length one takes the source's (step B5).
                let bytes = self.buffer_of(v)?;
                // Every byte of the target is written, whatever the source's size: charged
                // before, a unit each 64 (the review of B1, A1).
                let len = match self.heap.get(obj) {
                    Ok(Object::Buffer(b)) => b.len(),
                    _ => 0,
                };
                #[cfg(test)]
                let keep = crate::tests::planted(crate::tests::Fault::KeepZeroLength);
                #[cfg(not(test))]
                let keep = false;
                if len == 0 && !keep {
                    return self
                        .heap
                        .replace(obj, Object::Buffer(bytes), &mut self.budget)
                        .map(|()| None);
                }
                let r = self.charge(
                    u64::try_from(len / 64)
                        .unwrap_or(u64::MAX)
                        .saturating_add(1),
                );
                if r.is_ok()
                    && let Ok(Object::Buffer(b)) = self.heap.get_mut(obj)
                {
                    for (d, s) in b.iter_mut().zip(&bytes) {
                        *d = *s;
                    }
                    if let Some(rest) = b.get_mut(bytes.len()..) {
                        rest.fill(0);
                    }
                }
                mem::release(bytes, &mut self.budget);
                r.map(|()| None)
            }
            (Mode::Store, Target::Pkg) => match self.data(v)? {
                crate::convert::Data::Other(t) if t == crate::object::types::PACKAGE => {
                    let o = self.copy_object(v)?;
                    self.heap.replace(obj, o, &mut self.budget).map(|()| None)
                }
                _ => err(ErrorKind::Type),
            },
        }
    }

    /// Reads a buffer field (§19.3.5.7): an integer if it fits one, else a buffer.
    pub(crate) fn read_buffer_field(&mut self, f: &BufferField) -> Result<Val> {
        let bytes = usize::try_from(f.bit_len.div_ceil(8)).map_err(|_| ErrorKind::Range)?;
        let mut out = mem::zeroed(bytes.max(1), &mut self.budget)?;
        let got = match self.heap.get(f.source)? {
            Object::Buffer(b) | Object::String(b) => get_bits(b, f.bit_offset, f.bit_len, &mut out),
            _ => err(ErrorKind::Type),
        };
        if let Err(e) = got {
            mem::release(out, &mut self.budget);
            return Err(e);
        }
        if f.bit_len <= u64::from(self.width.bits()) {
            let mut x: u64 = 0;
            for (k, &b) in out.iter().take(8).enumerate() {
                x |= u64::from(b) << (k.saturating_mul(8) & 63);
            }
            mem::release(out, &mut self.budget);
            Ok(Val::Int(x))
        } else {
            out.truncate(bytes);
            self.new_buffer(out)
        }
    }

    /// Writes a buffer field, truncating or zero-extending `v`.
    pub(crate) fn write_buffer_field(&mut self, f: &BufferField, v: Val) -> Result<()> {
        let bytes = match v {
            Val::Obj(r) if matches!(self.heap.get(r)?, Object::String(_)) => {
                match self.heap.get(r)? {
                    Object::String(s) => mem::copy_bytes(s, &mut self.budget)?,
                    _ => Vec::new(),
                }
            }
            Val::Int(i) => mem::copy_bytes(&i.to_le_bytes(), &mut self.budget)?,
            other => self.buffer_of(other)?,
        };
        // The field's bytes are written one by one: charged before, a unit each 64 (the review
        // of B1, A1).
        let r = self
            .charge((f.bit_len / 8 / 64).saturating_add(1))
            .and_then(|()| match self.heap.get_mut(f.source)? {
                Object::Buffer(b) | Object::String(b) => {
                    set_bits(b, f.bit_offset, f.bit_len, &bytes)
                }
                _ => err(ErrorKind::Type),
            });
        mem::release(bytes, &mut self.budget);
        r
    }

    /// A store to `Debug`: the value's text to the host.
    fn debug(&mut self, v: Val) -> Result<()> {
        let text = match v {
            Val::Int(i) => self.int_hex(i)?,
            Val::Obj(r) => match self.heap.get(r)? {
                Object::String(s) => mem::copy_bytes(s, &mut self.budget)?,
                Object::Buffer(_) | Object::Integer(_) => self.string_of(v)?,
                o => mem::copy_bytes(Object::type_text(o.type_code()), &mut self.budget)?,
            },
            Val::Ref(_) => mem::copy_bytes(b"[Object Reference]", &mut self.budget)?,
            Val::None => mem::copy_bytes(b"[Uninitialized Object]", &mut self.budget)?,
        };
        self.host.debug(&text);
        mem::release(text, &mut self.budget);
        Ok(())
    }
}
