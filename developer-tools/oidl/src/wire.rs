//! An independent model of the wire format (`docs/spec/OIDL.md` §7, §10): a table walker over
//! the validated model, sharing nothing with the generated decoders but the layout arithmetic.
//! The hostile corpus is labelled by this file, and the generated decoders must agree with
//! every line of it.

use crate::layout::{self, InlineKind};
use crate::model::{HandleType, Method, Param, Struct, Type};

/// Why a message is refused (§7.5), in the corpus's spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// `size`
    Size,
    /// `wire`
    Wire,
    /// `kind`
    Kind,
    /// `direction`
    Direction,
    /// `reserved`
    Reserved,
    /// `txid`
    Txid,
    /// `ordinal`
    Ordinal,
    /// `padding`
    Padding,
    /// `bool`
    Bool,
    /// `enum`
    Enum,
    /// `bits`
    Bits,
    /// `nonzero`
    Nonzero,
    /// `count`
    Count,
    /// `utf8`
    Utf8,
    /// `nul`
    Nul,
    /// `handle_count`
    HandleCount,
    /// `handle_slot`
    HandleSlot,
    /// `handle_type`
    HandleType,
    /// `handle_rights`
    HandleRights,
}

impl Reason {
    /// The reason's name in the corpus.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Wire => "wire",
            Self::Kind => "kind",
            Self::Direction => "direction",
            Self::Reserved => "reserved",
            Self::Txid => "txid",
            Self::Ordinal => "ordinal",
            Self::Padding => "padding",
            Self::Bool => "bool",
            Self::Enum => "enum",
            Self::Bits => "bits",
            Self::Nonzero => "nonzero",
            Self::Count => "count",
            Self::Utf8 => "utf8",
            Self::Nul => "nul",
            Self::HandleCount => "handle_count",
            Self::HandleSlot => "handle_slot",
            Self::HandleType => "handle_type",
            Self::HandleRights => "handle_rights",
        }
    }
}

/// What the kernel reports about a received handle (§7.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandleInfo {
    /// Its object type.
    pub object: u32,
    /// Its rights.
    pub rights: u32,
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let raw: [u8; 2] = bytes.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(raw))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

/// Reads an integer type `size` bytes wide at `at`, sign-extended if `signed`, as `i128`.
fn read_int(bytes: &[u8], at: usize, size: u64, signed: bool) -> Option<i128> {
    let size = usize::try_from(size).ok()?;
    let raw = bytes.get(at..at.checked_add(size)?)?;
    let mut value: u128 = 0;
    for (i, byte) in raw.iter().enumerate() {
        value |= u128::from(*byte) << (8usize.saturating_mul(i));
    }
    if !signed {
        return i128::try_from(value).ok();
    }
    let bits = 8usize.saturating_mul(size);
    let sign_bit = 1u128 << (bits.saturating_sub(1));
    let negative = value & sign_bit != 0;
    let mut signed_value = i128::try_from(value).ok()?;
    if negative {
        signed_value = signed_value.checked_sub(1i128.checked_shl(u32::try_from(bits).ok()?)?)?;
    }
    Some(signed_value)
}

fn check_zero(bytes: &[u8], at: u64, len: u64) -> Result<(), Reason> {
    let at = usize::try_from(at).map_err(|_| Reason::Size)?;
    let len = usize::try_from(len).map_err(|_| Reason::Size)?;
    let end = at.checked_add(len).ok_or(Reason::Size)?;
    match bytes.get(at..end) {
        Some(region) if region.iter().all(|b| *b == 0) => Ok(()),
        Some(_) => Err(Reason::Padding),
        None => Err(Reason::Size),
    }
}

/// Walks a fixed-size value at `at` in `bytes`.
fn check_fixed(ty: &Type, bytes: &[u8], at: u64, minor: u16, library: &str) -> Result<(), Reason> {
    // An enum's or bits' members are versioned by its own library; only this library's minor
    // was negotiated (§9).
    let minor = if type_library(ty).is_some_and(|l| l != library) {
        u16::MAX
    } else {
        minor
    };
    let offset = usize::try_from(at).map_err(|_| Reason::Size)?;
    match ty {
        Type::Bool => match bytes.get(offset) {
            Some(0 | 1) => Ok(()),
            Some(_) => Err(Reason::Bool),
            None => Err(Reason::Size),
        },
        Type::Int(_) => {
            let (size, _) = layout::fixed_layout(ty);
            let end = offset
                .checked_add(usize::try_from(size).map_err(|_| Reason::Size)?)
                .ok_or(Reason::Size)?;
            if bytes.get(offset..end).is_some() {
                Ok(())
            } else {
                Err(Reason::Size)
            }
        }
        Type::Enum(e) => {
            let value =
                read_int(bytes, offset, e.repr.size(), e.repr.signed()).ok_or(Reason::Size)?;
            let known = e
                .members
                .iter()
                .any(|m| m.value == value && m.since.minor <= minor);
            if known { Ok(()) } else { Err(Reason::Enum) }
        }
        Type::Bits(b) => {
            let value = read_int(bytes, offset, b.repr.size(), false).ok_or(Reason::Size)?;
            let value = u64::try_from(value).map_err(|_| Reason::Size)?;
            if value & !b.known_mask(minor) == 0 {
                Ok(())
            } else {
                Err(Reason::Bits)
            }
        }
        Type::Struct(s) => check_struct(s, bytes, at, minor),
        Type::Array(elem, count) => {
            let (size, _) = layout::fixed_layout(elem);
            let mut index = 0u64;
            while index < u64::from(*count) {
                check_fixed(
                    elem,
                    bytes,
                    at.saturating_add(index.saturating_mul(size)),
                    minor,
                    library,
                )?;
                index = index.saturating_add(1);
            }
            Ok(())
        }
        Type::Vector(..) | Type::String(_) | Type::Handle(_) => Err(Reason::Size),
    }
}

/// Walks a plain struct: fields and padding in ascending offset (§7.5 step 6).
fn check_struct(s: &Struct, bytes: &[u8], at: u64, minor: u16) -> Result<(), Reason> {
    #[derive(Clone, Copy)]
    enum Item<'a> {
        Pad(u32, u32),
        Field(&'a crate::model::Field),
    }
    let mut items: Vec<(u32, Item<'_>)> = s
        .fields
        .iter()
        .map(|f| (f.offset, Item::Field(f)))
        .chain(s.padding.iter().map(|(o, l)| (*o, Item::Pad(*o, *l))))
        .collect();
    items.sort_by_key(|(offset, _)| *offset);
    for (_, item) in items {
        match item {
            Item::Pad(offset, len) => {
                check_zero(bytes, at.saturating_add(u64::from(offset)), u64::from(len))?;
            }
            Item::Field(field) => {
                check_fixed(
                    &field.ty,
                    bytes,
                    at.saturating_add(u64::from(field.offset)),
                    minor,
                    &s.library,
                )?;
            }
        }
    }
    Ok(())
}

/// The handles still to be consumed.
struct Handles<'a> {
    list: &'a [HandleInfo],
    used: usize,
}

impl Handles<'_> {
    fn take(&mut self, slot: u32, handle: &HandleType) -> Result<(), Reason> {
        if handle.optional && slot == u32::MAX {
            return Ok(());
        }
        if usize::try_from(slot).ok() != Some(self.used) {
            return Err(Reason::HandleSlot);
        }
        let info = *self.list.get(self.used).ok_or(Reason::HandleCount)?;
        self.used = self.used.saturating_add(1);
        if handle.object_value != 0 && u64::from(info.object) != handle.object_value {
            return Err(Reason::HandleType);
        }
        if u64::from(info.rights) & handle.rights_mask != handle.rights_mask {
            return Err(Reason::HandleRights);
        }
        Ok(())
    }
}

/// The library that declares `ty`, if it is an item of one.
fn type_library(ty: &Type) -> Option<&str> {
    match ty {
        Type::Enum(e) => Some(&e.library),
        Type::Bits(b) => Some(&b.library),
        Type::Struct(s) => Some(&s.library),
        Type::Array(elem, _) | Type::Vector(elem, _) => type_library(elem),
        _ => None,
    }
}

fn check_inline_nonzero(param: &Param, bytes: &[u8], at: u64) -> Result<(), Reason> {
    if !param.nonzero {
        return Ok(());
    }
    let offset = usize::try_from(at).map_err(|_| Reason::Size)?;
    let (size, _) = layout::fixed_layout(&param.ty);
    let end = offset
        .checked_add(usize::try_from(size).map_err(|_| Reason::Size)?)
        .ok_or(Reason::Size)?;
    let region = bytes.get(offset..end).ok_or(Reason::Size)?;
    if region.iter().all(|b| *b == 0) {
        Err(Reason::Nonzero)
    } else {
        Ok(())
    }
}

/// Checks a body at `minor` (§7.5 steps 5–9).
fn check_body(
    params: &[Param],
    body: &[u8],
    handles: &[HandleInfo],
    minor: u16,
    library: &str,
) -> Result<(), Reason> {
    let layout = layout::message_layout(params, minor);
    if (body.len() as u64) < layout.inline_size {
        return Err(Reason::Size);
    }
    let mut handle_list = Handles {
        list: handles,
        used: 0,
    };
    // Inline, by ascending offset.
    enum Step {
        Pad(u64, u64),
        Placed(usize),
    }
    let mut steps: Vec<(u64, Step)> = layout
        .placed
        .iter()
        .enumerate()
        .map(|(i, p)| (p.offset, Step::Placed(i)))
        .collect();
    steps.extend(layout.padding.iter().map(|(a, l)| (*a, Step::Pad(*a, *l))));
    steps.sort_by_key(|(at, _)| *at);
    let mut counts = Vec::new();
    for (_, step) in steps {
        match step {
            Step::Pad(at, len) => check_zero(body, at, len)?,
            Step::Placed(index) => {
                let placed = *layout.placed.get(index).ok_or(Reason::Size)?;
                let param = params.get(placed.param).ok_or(Reason::Size)?;
                let at = usize::try_from(placed.offset).map_err(|_| Reason::Size)?;
                match placed.kind {
                    InlineKind::Fixed => {
                        check_fixed(&param.ty, body, placed.offset, minor, library)?;
                        check_inline_nonzero(param, body, placed.offset)?;
                    }
                    InlineKind::Count => {
                        let count = le_u32(body, at).ok_or(Reason::Size)?;
                        let bound = match &param.ty {
                            Type::Vector(_, bound) | Type::String(bound) => *bound,
                            _ => 0,
                        };
                        if count > bound {
                            return Err(Reason::Count);
                        }
                        counts.push(count);
                    }
                    InlineKind::Slot => {
                        let slot = le_u32(body, at).ok_or(Reason::Size)?;
                        if let Type::Handle(handle) = &param.ty {
                            handle_list.take(slot, handle)?;
                        }
                    }
                }
            }
        }
    }
    // Exact size (§7.3).
    if body.len() as u64 != layout::expected_body_size(params, &layout, &counts) {
        return Err(Reason::Size);
    }
    // The variable part.
    let mut at = layout.inline_size;
    for (index, count) in layout.variable.iter().zip(&counts) {
        let param = params.get(*index).ok_or(Reason::Size)?;
        let element = layout::element_size(&param.ty);
        let len = element.saturating_mul(u64::from(*count));
        match &param.ty {
            Type::String(_) => {
                let from = usize::try_from(at).map_err(|_| Reason::Size)?;
                let to = usize::try_from(at.saturating_add(len)).map_err(|_| Reason::Size)?;
                let text = body.get(from..to).ok_or(Reason::Size)?;
                let valid = core::str::from_utf8(text).map_err(|_| Reason::Utf8)?;
                if valid.as_bytes().contains(&0) {
                    return Err(Reason::Nul);
                }
            }
            Type::Vector(elem, _) => {
                if let Type::Handle(handle) = elem.as_ref() {
                    let mut i = 0u64;
                    while i < u64::from(*count) {
                        let slot_at = at.saturating_add(i.saturating_mul(4));
                        let slot_at = usize::try_from(slot_at).map_err(|_| Reason::Size)?;
                        let slot = le_u32(body, slot_at).ok_or(Reason::Size)?;
                        handle_list.take(slot, handle)?;
                        i = i.saturating_add(1);
                    }
                } else {
                    let mut i = 0u64;
                    while i < u64::from(*count) {
                        check_fixed(
                            elem,
                            body,
                            at.saturating_add(i.saturating_mul(element)),
                            minor,
                            library,
                        )?;
                        i = i.saturating_add(1);
                    }
                }
            }
            _ => {}
        }
        check_zero(
            body,
            at.saturating_add(len),
            layout::round_up(len, 8).saturating_sub(len),
        )?;
        at = at.saturating_add(layout::round_up(len, 8));
    }
    if handle_list.used != handles.len() {
        return Err(Reason::HandleCount);
    }
    Ok(())
}

/// The model's verdict on a whole message (§7.5).
pub fn check_message(
    method: &Method,
    library: &str,
    request: bool,
    minor: u16,
    message: &[u8],
    handles: &[HandleInfo],
) -> Result<(), Reason> {
    if message.len() < 16 {
        return Err(Reason::Size);
    }
    let size = le_u32(message, 0).ok_or(Reason::Size)?;
    if u64::from(size) != message.len() as u64 || message.len() & 7 != 0 {
        return Err(Reason::Size);
    }
    if message.get(4) != Some(&1) {
        return Err(Reason::Wire);
    }
    let kind = *message.get(5).ok_or(Reason::Size)?;
    if !(1..=3).contains(&kind) {
        return Err(Reason::Kind);
    }
    if le_u16(message, 6).ok_or(Reason::Size)? != 0 {
        return Err(Reason::Reserved);
    }
    // The corpus decodes a message addressed to one method, so the kind must be its direction.
    let expected_kind = if request { 1 } else { 2 };
    if kind != expected_kind {
        return Err(Reason::Direction);
    }
    let ordinal = le_u32(message, 8).ok_or(Reason::Size)?;
    let txid = le_u32(message, 12).ok_or(Reason::Size)?;
    let txid_ok = if request && method.oneway {
        txid == 0
    } else {
        txid != 0
    };
    if !txid_ok {
        return Err(Reason::Txid);
    }
    if ordinal != method.ordinal {
        return Err(Reason::Ordinal);
    }
    let body = message.get(16..).ok_or(Reason::Size)?;
    let params = if request {
        &method.params
    } else {
        &method.returns
    };
    check_body(params, body, handles, minor, library)
}

/// The model's verdict on one system call's registers (§10): `Ok` or the status name it fails
/// with. The kernel's own table lookups and pointer checks come after this.
pub fn check_call(method: &Method, holds: bool, args: [u64; 6]) -> Result<(), String> {
    if method.reserved {
        return Err("not_supported".to_string());
    }
    if let Some(requirement) = &method.requires
        && !holds
    {
        return Err(requirement.denied.name.clone());
    }
    let mut index = 0usize;
    let next = |index: &mut usize| {
        let value = args.get(*index).copied().unwrap_or(0);
        *index = index.saturating_add(1);
        value
    };
    let invalid = || Err("invalid_args".to_string());
    for param in &method.params {
        match &param.ty {
            Type::Handle(handle) => {
                let raw = next(&mut index);
                if handle.optional && raw == 0 {
                    continue;
                }
                if raw > u64::from(u32::MAX) {
                    return Err("bad_handle".to_string());
                }
            }
            Type::Bool => {
                if next(&mut index) > 1 {
                    return invalid();
                }
            }
            Type::Int(int) => {
                let raw = next(&mut index);
                let as_i128 = if int.signed() {
                    i128::from(raw as i64)
                } else {
                    i128::from(raw)
                };
                if as_i128 < int.min() || as_i128 > int.max() {
                    return invalid();
                }
                if param.nonzero && raw == 0 {
                    return invalid();
                }
            }
            Type::Enum(e) => {
                let raw = next(&mut index);
                let value = if e.repr.signed() {
                    i128::from(raw as i64)
                } else {
                    i128::from(raw)
                };
                if !e.members.iter().any(|m| m.value == value) {
                    return invalid();
                }
            }
            Type::Bits(b) => {
                let raw = next(&mut index);
                if i128::from(raw) > b.repr.max()
                    || raw & !b.known_mask(u16::MAX) != 0
                    || (param.nonzero && raw == 0)
                {
                    return invalid();
                }
            }
            Type::Vector(_, bound) | Type::String(bound) => {
                let _pointer = next(&mut index);
                let count = next(&mut index);
                if count > u64::from(*bound) {
                    return invalid();
                }
            }
            Type::Struct(_) | Type::Array(..) => {
                let _pointer = next(&mut index);
            }
        }
    }
    // Results take pointers; a capacity above its bound is clamped, not refused (§10).
    Ok(())
}
