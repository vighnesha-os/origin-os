//! Conversions between data types (ACPI 6.6 §19.3.5, Tables 19.6 and 19.7), and the text forms
//! the explicit conversion operators make.
//!
//! The text forms are G4's strict choices, provisional until the reading session answers
//! (`docs/research/2026-11-14-aml-interpreter.md` §9): integers as the integer width's
//! hexadecimal digits, upper case, no prefix; a buffer as two upper-case digits a byte, separated
//! by spaces (implicit) or commas (`ToHexString`); `ToDecimalString` of a buffer as decimal values
//! separated by commas.

use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::Interpreter;
use crate::error::{ErrorKind, Result, err};
use crate::host::Host;
use crate::mem;
use crate::object::{Object, Val};

/// What a value is, for operators whose first operand decides (§19.3.5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Data {
    Int,
    Str,
    Buf,
    /// Anything else, by its type code.
    Other(u64),
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn hex_digit(n: u8) -> u8 {
    HEX.get(usize::from(n & 0xF)).copied().unwrap_or(b'0')
}

fn hex_value(c: u8) -> Option<u64> {
    match c {
        b'0'..=b'9' => Some(u64::from(c.wrapping_sub(b'0'))),
        b'a'..=b'f' => Some(u64::from(c.wrapping_sub(b'a')).saturating_add(10)),
        b'A'..=b'F' => Some(u64::from(c.wrapping_sub(b'A')).saturating_add(10)),
        _ => None,
    }
}

impl<H: Host> Interpreter<H> {
    /// What `v` is.
    pub(crate) fn data(&self, v: Val) -> Result<Data> {
        Ok(match v {
            Val::Int(_) => Data::Int,
            Val::None => return err(ErrorKind::Uninitialized),
            Val::Ref(_) => Data::Other(0),
            Val::Obj(r) => match self.heap.get(r)? {
                Object::Integer(_) => Data::Int,
                Object::String(_) => Data::Str,
                Object::Buffer(_) => Data::Buf,
                o => Data::Other(o.type_code()),
            },
        })
    }

    /// `v` as an integer (Table 19.7), masked to the width.
    pub(crate) fn int_of(&mut self, v: Val) -> Result<u64> {
        let mask = self.width.mask();
        let bytes = self.width.bytes();
        let digits = bytes.saturating_mul(2);
        match v {
            Val::Int(i) => Ok(i & mask),
            Val::None => err(ErrorKind::Uninitialized),
            Val::Ref(_) => err(ErrorKind::Type),
            Val::Obj(r) => match self.heap.get(r)? {
                Object::Integer(i) => Ok(*i & mask),
                Object::Buffer(b) => {
                    if b.is_empty() {
                        return err(ErrorKind::Type);
                    }
                    let mut x: u64 = 0;
                    for (k, &byte) in b.iter().take(bytes).enumerate() {
                        x |= u64::from(byte) << (k.saturating_mul(8) & 63);
                    }
                    Ok(x & mask)
                }
                Object::String(s) => {
                    // Hexadecimal digits from the first, to the first other character or the
                    // width; no `0x` prefix; an empty string refused.
                    if s.is_empty() {
                        return err(ErrorKind::Type);
                    }
                    let mut x: u64 = 0;
                    for &c in s.iter().take(digits) {
                        let Some(d) = hex_value(c) else { break };
                        x = (x << 4) | d;
                    }
                    Ok(x & mask)
                }
                _ => err(ErrorKind::Type),
            },
        }
    }

    /// The bytes of an integer at the width, low first.
    pub(crate) fn int_bytes(&mut self, i: u64) -> Result<Vec<u8>> {
        let all = i.to_le_bytes();
        mem::copy_bytes(
            all.get(..self.width.bytes()).unwrap_or(&all),
            &mut self.budget,
        )
    }

    /// `v` as a buffer's bytes (Table 19.7): a string's characters and its NUL.
    pub(crate) fn buffer_of(&mut self, v: Val) -> Result<Vec<u8>> {
        match v {
            Val::Int(i) => self.int_bytes(i),
            Val::None => err(ErrorKind::Uninitialized),
            Val::Ref(_) => err(ErrorKind::Type),
            Val::Obj(r) => match self.heap.get(r)? {
                Object::Integer(i) => {
                    let i = *i;
                    self.int_bytes(i)
                }
                Object::Buffer(b) => mem::copy_bytes(b, &mut self.budget),
                Object::String(s) => {
                    if s.is_empty() {
                        return Ok(Vec::new());
                    }
                    let mut out = mem::with_capacity(s.len().saturating_add(1), &mut self.budget)?;
                    mem::extend_bytes(&mut out, s, &mut self.budget)?;
                    mem::extend_bytes(&mut out, &[0], &mut self.budget)?;
                    Ok(out)
                }
                _ => err(ErrorKind::Type),
            },
        }
    }

    /// The hexadecimal text of `i` at the width.
    pub(crate) fn int_hex(&mut self, i: u64) -> Result<Vec<u8>> {
        let digits = self.width.bytes().saturating_mul(2);
        let mut out = mem::with_capacity(digits, &mut self.budget)?;
        for k in (0..digits).rev() {
            let shift = u32::try_from(k.saturating_mul(4)).unwrap_or(0) & 63;
            let nibble = (i >> shift) & 0xF;
            mem::extend_bytes(
                &mut out,
                &[hex_digit(u8::try_from(nibble).unwrap_or(0))],
                &mut self.budget,
            )?;
        }
        Ok(out)
    }

    /// Bytes as two hexadecimal digits each, separated by `sep`.
    pub(crate) fn bytes_hex(&mut self, bytes: &[u8], sep: u8) -> Result<Vec<u8>> {
        let len = bytes.len().saturating_mul(3);
        let mut out = mem::with_capacity(len, &mut self.budget)?;
        for (k, &b) in bytes.iter().enumerate() {
            if k > 0 {
                mem::extend_bytes(&mut out, &[sep], &mut self.budget)?;
            }
            mem::extend_bytes(
                &mut out,
                &[hex_digit(b >> 4), hex_digit(b)],
                &mut self.budget,
            )?;
        }
        Ok(out)
    }

    /// The decimal text of `i`.
    pub(crate) fn int_decimal(&mut self, i: u64, out: &mut Vec<u8>) -> Result<()> {
        let mut digits = [0u8; 20];
        let mut n = i;
        let mut len: usize = 0;
        loop {
            if let Some(slot) = digits.get_mut(len) {
                *slot = b'0'.saturating_add(u8::try_from(n % 10).unwrap_or(0));
            }
            len = len.saturating_add(1);
            n /= 10;
            if n == 0 || len >= digits.len() {
                break;
            }
        }
        let used = digits.get(..len).unwrap_or(&[]);
        for &d in used.iter().rev() {
            mem::extend_bytes(out, &[d], &mut self.budget)?;
        }
        Ok(())
    }

    /// `v` as a string's characters (Table 19.7).
    pub(crate) fn string_of(&mut self, v: Val) -> Result<Vec<u8>> {
        match v {
            Val::Int(i) => self.int_hex(i),
            Val::None => err(ErrorKind::Uninitialized),
            Val::Ref(_) => err(ErrorKind::Type),
            Val::Obj(r) => match self.heap.get(r)? {
                Object::Integer(i) => {
                    let i = *i;
                    self.int_hex(i)
                }
                Object::String(s) => mem::copy_bytes(s, &mut self.budget),
                Object::Buffer(b) => {
                    let b = mem::copy_bytes(b, &mut self.budget)?;
                    let out = self.bytes_hex(&b, b' ');
                    mem::release(b, &mut self.budget);
                    out
                }
                _ => err(ErrorKind::Type),
            },
        }
    }

    /// Compares `a` with `b` converted to `a`'s type (§19.6.70–76): numerically for integers,
    /// byte by byte, unsigned, for strings and buffers, the shorter being less when it is a prefix.
    pub(crate) fn compare(&mut self, a: Val, b: Val) -> Result<Ordering> {
        match self.data(a)? {
            Data::Int => {
                let x = self.int_of(a)?;
                let y = self.int_of(b)?;
                Ok(x.cmp(&y))
            }
            Data::Str => {
                let x = self.string_of(a)?;
                let y = self.string_of(b);
                let order = y
                    .as_ref()
                    .map(|y| x.as_slice().cmp(y.as_slice()))
                    .map_err(|e| *e);
                mem::release(x, &mut self.budget);
                if let Ok(y) = y {
                    mem::release(y, &mut self.budget);
                }
                order
            }
            Data::Buf => {
                let x = self.buffer_of(a)?;
                let y = self.buffer_of(b);
                let order = y
                    .as_ref()
                    .map(|y| x.as_slice().cmp(y.as_slice()))
                    .map_err(|e| *e);
                mem::release(x, &mut self.budget);
                if let Ok(y) = y {
                    mem::release(y, &mut self.budget);
                }
                order
            }
            Data::Other(_) => err(ErrorKind::Type),
        }
    }

    /// A new string object.
    pub(crate) fn new_string(&mut self, bytes: Vec<u8>) -> Result<Val> {
        Ok(Val::Obj(
            self.heap.alloc(Object::String(bytes), &mut self.budget)?,
        ))
    }

    /// A new buffer object.
    pub(crate) fn new_buffer(&mut self, bytes: Vec<u8>) -> Result<Val> {
        Ok(Val::Obj(
            self.heap.alloc(Object::Buffer(bytes), &mut self.budget)?,
        ))
    }

    /// `ToInteger`'s explicit conversion (§19.6.141): a string must be all decimal digits, or
    /// `0x` and hexadecimal digits, and fit the width; a buffer gives its first bytes.
    pub(crate) fn explicit_int(&mut self, v: Val) -> Result<u64> {
        let Val::Obj(r) = v else {
            return self.int_of(v);
        };
        let Object::String(s) = self.heap.get(r)? else {
            return self.int_of(v);
        };
        let (digits, radix) = match s.as_slice() {
            [b'0', b'x' | b'X', rest @ ..] => (rest, 16u64),
            all => (all, 10u64),
        };
        if digits.is_empty() {
            return err(ErrorKind::Type);
        }
        // Leading zeros keep the loop going for the whole string: a unit for each 64 digits.
        self.note_work(
            u64::try_from(digits.len() / 64)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        );
        let mut x: u64 = 0;
        for &c in digits {
            let d = hex_value(c).filter(|&d| d < radix).ok_or(ErrorKind::Type)?;
            x = x
                .checked_mul(radix)
                .and_then(|x| x.checked_add(d))
                .ok_or(ErrorKind::Type)?;
        }
        if x & !self.width.mask() != 0 {
            return err(ErrorKind::Type);
        }
        Ok(x)
    }
}
