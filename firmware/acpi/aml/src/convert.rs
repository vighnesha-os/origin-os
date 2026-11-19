//! Conversions between data types (ACPI 6.6 §19.3.5, Tables 19.6 and 19.7), and the text forms
//! the explicit conversion operators make.
//!
//! The text forms are ACPICA's (G4, TASKS 64's ledger §3; step B5), upper-case digits throughout:
//! an integer, implicitly, as the integer width's hexadecimal digits with leading zeros and no
//! prefix; `ToHexString` of an integer as `0x` and its digits without leading zeros (`0x0`); a
//! buffer as `0x` and two digits a byte, separated by a space (implicit) or a comma
//! (`ToHexString`); `ToDecimalString` as decimal, a buffer's bytes separated by commas; an empty
//! buffer as an empty string. A string becomes an integer without ever failing: leading white
//! space and zeros skipped, hexadecimal (a `0x` tolerated) implicitly, decimal for `ToInteger`
//! unless `0x`; it stops at the first character that is not a digit, or at the digit that would
//! overflow the integer width, keeping the value so far; an empty string gives 0. A string becomes
//! a buffer with its terminating NUL, an empty one as one zero byte.

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

/// White space a string-to-integer conversion skips first: space, and 0x09 to 0x0D (B5's
/// contract, Q8).
const fn is_space(c: u8) -> bool {
    c == b' ' || matches!(c, 0x09..=0x0D)
}

/// The digits of `s` from the first after white space, an optional `0x` (`hex` is then forced on
/// when `prefix_sets_hex`), and leading zeros; and the radix.
fn digits_of(s: &[u8], hex: bool) -> (&[u8], u64) {
    let start = s.iter().position(|&c| !is_space(c)).unwrap_or(s.len());
    let rest = s.get(start..).unwrap_or(&[]);
    let (rest, radix) = match rest {
        [b'0', b'x' | b'X', tail @ ..] => (tail, 16u64),
        all => (all, if hex { 16u64 } else { 10u64 }),
    };
    let first = rest.iter().position(|&c| c != b'0').unwrap_or(rest.len());
    (rest.get(first..).unwrap_or(&[]), radix)
}

/// `digits` in `radix`, up to the first character that is not one and before the digit that would
/// overflow `mask`: never an error (G4, step B5).
fn total_int(digits: &[u8], radix: u64, mask: u64) -> u64 {
    let mut x: u64 = 0;
    for &c in digits {
        let Some(d) = hex_value(c).filter(|&d| d < radix) else {
            break;
        };
        match x.checked_mul(radix).and_then(|x| x.checked_add(d)) {
            Some(n) if n & !mask == 0 => x = n,
            _ => break,
        }
    }
    x
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
                    // Hexadecimal, a `0x` tolerated, never failing (G4, step B5).
                    let s = s.as_slice();
                    self.note_work(
                        u64::try_from(s.len() / 64)
                            .unwrap_or(u64::MAX)
                            .saturating_add(1),
                    );
                    let (d, radix) = digits_of(s, true);
                    Ok(total_int(d, radix, mask))
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
                    // Its terminating NUL too, an empty string's included (G4, step B5; B5's
                    // contract, Q8).
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

    /// Bytes as `0x` and two hexadecimal digits each, separated by `sep` (G4, step B5).
    pub(crate) fn bytes_hex(&mut self, bytes: &[u8], sep: u8) -> Result<Vec<u8>> {
        let len = bytes.len().saturating_mul(5);
        let mut out = mem::with_capacity(len, &mut self.budget)?;
        #[cfg(test)]
        let prefix: &[u8] = if crate::tests::planted(crate::tests::Fault::OldTexts) {
            b""
        } else {
            b"0x"
        };
        #[cfg(not(test))]
        let prefix: &[u8] = b"0x";
        for (k, &b) in bytes.iter().enumerate() {
            if k > 0 {
                mem::extend_bytes(&mut out, &[sep], &mut self.budget)?;
            }
            mem::extend_bytes(&mut out, prefix, &mut self.budget)?;
            mem::extend_bytes(
                &mut out,
                &[hex_digit(b >> 4), hex_digit(b)],
                &mut self.budget,
            )?;
        }
        Ok(out)
    }

    /// `ToHexString`'s text of an integer: `0x` and its digits without leading zeros, so zero is
    /// `0x0` (G4, step B5).
    pub(crate) fn int_hex_short(&mut self, i: u64) -> Result<Vec<u8>> {
        let mut out = mem::with_capacity(18, &mut self.budget)?;
        mem::extend_bytes(&mut out, b"0x", &mut self.budget)?;
        let mut started = false;
        for k in (0..16u32).rev() {
            let nibble = (i >> (k.saturating_mul(4) & 63)) & 0xF;
            if nibble == 0 && !started && k > 0 {
                continue;
            }
            started = true;
            mem::extend_bytes(
                &mut out,
                &[hex_digit(u8::try_from(nibble).unwrap_or(0))],
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
        // Decimal unless `0x`, never failing (G4, step B5). The scan of white space and zeros
        // reads the whole string at most: a unit for each 64 bytes.
        let s = s.as_slice();
        self.note_work(
            u64::try_from(s.len() / 64)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        );
        let (digits, radix) = digits_of(s, false);
        Ok(total_int(digits, radix, self.width.mask()))
    }
}
