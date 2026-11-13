//! Layout rules (`docs/spec/OIDL.md` §5, §7.2): the only place offsets are computed, so a
//! generator can never disagree with a validator about where a field is. Sizes are computed in
//! `u64` with saturation; every result is compared against a limit before it is believed.

use crate::model::{Method, Param, Struct, Type};

/// A handle's width in bits wherever a handle's value crosses a boundary: a system-call
/// register, an out pointer, an element of a handle vector or array, and each handle a transport
/// delivers with a message (§10, §7.4; ADR-0005 Amendment 6, E1). A slot in a `channel` message
/// is a position, not a handle, and is 4 bytes whatever this is. The generator emits handles of
/// this width and the canonical description records it (§13, decision item 237), so neither can
/// change without the other.
pub const HANDLE_BITS: u32 = 64;

/// Size of the message header (§7.1).
pub const HEADER_SIZE: u64 = 16;
/// Every message is aligned on this.
pub const MESSAGE_ALIGN: u64 = 8;

/// `value` rounded up to the next multiple of `align`, a power of two.
pub fn round_up(value: u64, align: u64) -> u64 {
    let mask = align.wrapping_sub(1);
    value.saturating_add(mask) & !mask
}

/// Size and alignment of a fixed-size type. `(0, 1)` for one that is not fixed-size, which the
/// validator never lets through to a position that needs a layout.
pub fn fixed_layout(ty: &Type) -> (u64, u64) {
    match ty {
        Type::Bool => (1, 1),
        Type::Int(int) => (int.size(), int.size()),
        Type::Enum(e) => (e.repr.size(), e.repr.size()),
        Type::Bits(b) => (b.repr.size(), b.repr.size()),
        Type::Struct(s) => (u64::from(s.size), u64::from(s.align)),
        Type::Array(elem, count) => {
            let (size, align) = fixed_layout(elem);
            (size.saturating_mul(u64::from(*count)), align)
        }
        Type::Vector(..) | Type::String(_) | Type::Handle(_) => (0, 1),
    }
}

/// Whether `ty` is a fixed-size type (§3): the only kind that may be a struct field or an array
/// or vector element (a `handle` may be a vector element too).
pub fn is_fixed(ty: &Type) -> bool {
    match ty {
        Type::Bool | Type::Int(_) | Type::Enum(_) | Type::Bits(_) => true,
        Type::Struct(s) => !s.sized,
        Type::Array(elem, _) => is_fixed(elem),
        Type::Vector(..) | Type::String(_) | Type::Handle(_) => false,
    }
}

/// The element size of a `vector` or `string` on the wire.
pub fn element_size(ty: &Type) -> u64 {
    match ty {
        Type::Vector(elem, _) => match elem.as_ref() {
            Type::Handle(_) => 4,
            other => fixed_layout(other).0,
        },
        Type::String(_) => 1,
        _ => 0,
    }
}

/// What a parameter occupies in the inline part of a message (§7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineKind {
    /// Its own bytes.
    Fixed,
    /// A `u32 count`, for a vector or string.
    Count,
    /// A `u32` handle slot.
    Slot,
}

/// A parameter's position in the inline part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// The index of the parameter.
    pub param: usize,
    /// Offset from the start of the body.
    pub offset: u64,
    /// What is there.
    pub kind: InlineKind,
}

/// The layout of one message body at one minor (§7.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageLayout {
    /// Parameters present, in ascending offset.
    pub placed: Vec<Placed>,
    /// Inline padding `(offset, length)`, in ascending offset.
    pub padding: Vec<(u64, u64)>,
    /// Inline size, a multiple of 8.
    pub inline_size: u64,
    /// The indexes of the vectors and strings present, in order.
    pub variable: Vec<usize>,
}

fn inline_form(ty: &Type) -> (InlineKind, u64, u64) {
    match ty {
        Type::Vector(..) | Type::String(_) => (InlineKind::Count, 4, 4),
        Type::Handle(_) => (InlineKind::Slot, 4, 4),
        other => {
            let (size, align) = fixed_layout(other);
            (InlineKind::Fixed, size, align)
        }
    }
}

/// The layout of `params` at `minor`.
pub fn message_layout(params: &[Param], minor: u16) -> MessageLayout {
    let mut placed = Vec::new();
    let mut padding = Vec::new();
    let mut variable = Vec::new();
    let mut offset: u64 = 0;
    let mut group: Option<u16> = None;
    for (index, param) in params.iter().enumerate() {
        if param.since.minor > minor {
            continue;
        }
        if group.is_some_and(|g| g < param.since.minor) {
            // A new version's parameters start after the previous version's whole inline
            // part, so each version is a prefix of the next (§7.2).
            let start = round_up(offset, MESSAGE_ALIGN);
            if start > offset {
                padding.push((offset, start.saturating_sub(offset)));
            }
            offset = start;
        }
        group = Some(param.since.minor);
        let (kind, size, align) = inline_form(&param.ty);
        let at = round_up(offset, align);
        if at > offset {
            padding.push((offset, at.saturating_sub(offset)));
        }
        placed.push(Placed {
            param: index,
            offset: at,
            kind,
        });
        offset = at.saturating_add(size);
        if kind == InlineKind::Count {
            variable.push(index);
        }
    }
    let inline_size = round_up(offset, MESSAGE_ALIGN);
    if inline_size > offset {
        padding.push((offset, inline_size.saturating_sub(offset)));
    }
    MessageLayout {
        placed,
        padding,
        inline_size,
        variable,
    }
}

/// The body's bytes at `minor`, given each present vector's and string's count, in the order of
/// `layout.variable`.
pub fn expected_body_size(params: &[Param], layout: &MessageLayout, counts: &[u32]) -> u64 {
    let mut total = layout.inline_size;
    for (index, count) in layout.variable.iter().zip(counts) {
        let size = params
            .get(*index)
            .map_or(0, |p| element_size(&p.ty))
            .saturating_mul(u64::from(*count));
        total = total.saturating_add(round_up(size, MESSAGE_ALIGN));
    }
    total
}

/// The largest whole message, header included, of `params` at the newest minor.
pub fn max_message_size(params: &[Param], newest_minor: u16) -> u64 {
    let layout = message_layout(params, newest_minor);
    let mut total = HEADER_SIZE.saturating_add(layout.inline_size);
    for index in &layout.variable {
        if let Some(param) = params.get(*index) {
            let bound = match &param.ty {
                Type::Vector(_, bound) | Type::String(bound) => u64::from(*bound),
                _ => 0,
            };
            let bytes = element_size(&param.ty).saturating_mul(bound);
            total = total.saturating_add(round_up(bytes, MESSAGE_ALIGN));
        }
    }
    total
}

/// The most handles `params` can carry at the newest minor.
pub fn max_handles(params: &[Param], newest_minor: u16) -> u64 {
    params
        .iter()
        .filter(|p| p.since.minor <= newest_minor)
        .fold(0u64, |count, p| match &p.ty {
            Type::Handle(_) => count.saturating_add(1),
            Type::Vector(elem, bound) if matches!(elem.as_ref(), Type::Handle(_)) => {
                count.saturating_add(u64::from(*bound))
            }
            _ => count,
        })
}

/// Lays out one struct's fields (§5.3), filling in offsets, padding and version sizes. Returns
/// `(fields, size, align, padding, sizes)`.
///
/// `fields` carry their types and `since`; their `offset`s are overwritten.
pub fn struct_layout(mut fields: Vec<crate::model::Field>, sized: bool) -> Laid {
    let mut offset: u64 = if sized { 4 } else { 0 };
    let mut align: u64 = if sized { 4 } else { 1 };
    let mut padding = Vec::new();
    let mut sizes = Vec::new();
    let mut group: Option<u16> = None;
    for field in &mut fields {
        if group.is_some_and(|g| g < field.since.minor) {
            let previous = round_up(offset, align);
            if let Some(minor) = group {
                sizes.push((minor, previous));
            }
            if previous > offset {
                padding.push((offset, previous.saturating_sub(offset)));
            }
            offset = previous;
        }
        group = Some(field.since.minor);
        let (size, field_align) = fixed_layout(&field.ty);
        let at = round_up(offset, field_align);
        if at > offset {
            padding.push((offset, at.saturating_sub(offset)));
        }
        field.offset = u32::try_from(at).unwrap_or(u32::MAX);
        offset = at.saturating_add(size);
        align = align.max(field_align);
    }
    let size = round_up(offset, align);
    if size > offset {
        padding.push((offset, size.saturating_sub(offset)));
    }
    if let Some(minor) = group {
        sizes.push((minor, size));
    } else if sized {
        sizes.push((0, size));
    }
    Laid {
        fields,
        size,
        align,
        padding,
        sizes,
    }
}

/// What [`struct_layout`] computed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laid {
    /// The fields, with their offsets.
    pub fields: Vec<crate::model::Field>,
    /// Size at the newest version.
    pub size: u64,
    /// Alignment.
    pub align: u64,
    /// Padding `(offset, length)`.
    pub padding: Vec<(u64, u64)>,
    /// `(minor, size)` for each version of a sized struct, ascending.
    pub sizes: Vec<(u16, u64)>,
}

impl Struct {
    /// The size of this sized struct at `minor`.
    pub fn size_at(&self, minor: u16) -> u32 {
        self.sizes
            .iter()
            .rev()
            .find(|(m, _)| *m <= minor)
            .map_or(self.size, |(_, size)| *size)
    }

    /// The size of this sized struct at its base version.
    pub fn base_size(&self) -> u32 {
        self.sizes.first().map_or(self.size, |(_, size)| *size)
    }

    /// The alignment of this struct at `minor`: only the fields present then.
    pub fn align_at(&self, minor: u16) -> u32 {
        let start = if self.sized { 4 } else { 1 };
        self.fields
            .iter()
            .filter(|f| f.since.minor <= minor)
            .map(|f| u32::try_from(fixed_layout(&f.ty).1).unwrap_or(1))
            .fold(start, u32::max)
    }
}

impl Method {
    /// The requests's minors, from the method's `since` to `newest`.
    pub fn minors(&self, newest: u16) -> impl Iterator<Item = u16> + use<> {
        self.since.minor..=newest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Field, Int, Version};

    const BASE: Version = Version { major: 1, minor: 0 };

    fn field(name: &str, ty: Type, since: Version) -> Field {
        Field {
            name: name.to_string(),
            doc: Vec::new(),
            ty,
            since,
            offset: 0,
        }
    }

    fn param(name: &str, ty: Type, minor: u16) -> Param {
        Param {
            name: name.to_string(),
            doc: Vec::new(),
            ty,
            since: Version { major: 1, minor },
            nonzero: false,
            count: None,
            also_on: Vec::new(),
        }
    }

    #[test]
    fn round_up_is_correct() {
        assert_eq!(round_up(0, 8), 0);
        assert_eq!(round_up(1, 8), 8);
        assert_eq!(round_up(8, 8), 8);
        assert_eq!(round_up(9, 4), 12);
        assert_eq!(round_up(u64::MAX - 1, 8) & 7, 0, "saturates, never wraps");
    }

    #[test]
    fn a_struct_is_laid_out_with_natural_alignment_and_named_padding() {
        // u32 @0, u8 @4, pad @5..8, u64 @8; size 16, align 8.
        let laid = struct_layout(
            vec![
                field("a", Type::Int(Int::U32), BASE),
                field("b", Type::Bool, BASE),
                field("c", Type::Int(Int::U64), BASE),
            ],
            false,
        );
        let offsets: Vec<u32> = laid.fields.iter().map(|f| f.offset).collect();
        assert_eq!(offsets, [0, 4, 8]);
        assert_eq!((laid.size, laid.align), (16, 8));
        assert_eq!(laid.padding, [(5, 3)]);
        assert_eq!(laid.sizes, [(0, 16)], "a plain struct has one version");
    }

    #[test]
    fn trailing_padding_rounds_to_the_alignment() {
        let laid = struct_layout(
            vec![
                field("a", Type::Int(Int::U64), BASE),
                field("b", Type::Int(Int::U8), BASE),
            ],
            false,
        );
        assert_eq!((laid.size, laid.align), (16, 8));
        assert_eq!(laid.padding, [(9, 7)]);
    }

    #[test]
    fn a_sized_struct_starts_with_its_size_and_each_version_is_a_prefix() {
        let v1 = Version { major: 1, minor: 1 };
        let laid = struct_layout(
            vec![
                field("a", Type::Int(Int::U16), BASE),
                field("b", Type::Int(Int::U64), v1),
                field("c", Type::Int(Int::U8), v1),
            ],
            true,
        );
        // size @0..4, a @4..6, base rounds to 8 (align 4); b @8, c @16, then trailing to 24.
        let offsets: Vec<u32> = laid.fields.iter().map(|f| f.offset).collect();
        assert_eq!(offsets, [4, 8, 16]);
        assert_eq!(laid.sizes, [(0, 8), (1, 24)]);
        assert_eq!(laid.size, 24);
    }

    #[test]
    fn a_message_is_inline_then_variable_with_8_byte_ends() {
        let params = vec![
            param("flag", Type::Bool, 0),
            param("count", Type::Int(Int::U16), 0),
            param("name", Type::String(16), 0),
            param(
                "h",
                Type::Handle(crate::model::HandleType {
                    object: Some("channel".to_string()),
                    object_value: 4,
                    rights: vec!["read".to_string()],
                    rights_mask: 1,
                    optional: false,
                }),
                0,
            ),
        ];
        let layout = message_layout(&params, 0);
        let offsets: Vec<(u64, InlineKind)> =
            layout.placed.iter().map(|p| (p.offset, p.kind)).collect();
        // The worked example of the specification (§7): flag @0, pad @1, count @2, name count
        // @4, slot @8, pad @12..16.
        assert_eq!(
            offsets,
            [
                (0, InlineKind::Fixed),
                (2, InlineKind::Fixed),
                (4, InlineKind::Count),
                (8, InlineKind::Slot),
            ]
        );
        assert_eq!(layout.padding, [(1, 1), (12, 4)]);
        assert_eq!(layout.inline_size, 16);
        assert_eq!(expected_body_size(&params, &layout, &[2]), 16 + 8);
        assert_eq!(expected_body_size(&params, &layout, &[0]), 16);
        assert_eq!(max_message_size(&params, 0), 16 + 16 + 16);
        assert_eq!(max_handles(&params, 0), 1);
    }

    #[test]
    fn a_later_parameter_starts_after_the_previous_versions_whole_inline_part() {
        let params = vec![
            param("a", Type::Int(Int::U8), 0),
            param("b", Type::Int(Int::U8), 1),
        ];
        assert_eq!(message_layout(&params, 0).inline_size, 8);
        let newer = message_layout(&params, 1);
        assert_eq!(
            newer.placed[1].offset, 8,
            "not 1: minor 0's layout is a prefix"
        );
        assert_eq!(newer.inline_size, 16);
    }
}
