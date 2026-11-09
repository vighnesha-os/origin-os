//! The hostile corpus (`docs/spec/OIDL.md` §14): for every method, a valid message or register
//! set, and many derived cases, each labelled by the independent model in [`wire`].

use core::fmt::Write as _;
use std::collections::HashSet;

use crate::fingerprint::hex;
use crate::layout::{self, InlineKind};
use crate::model::{HandleType, Library, Method, Param, Struct, Transport, Type};
use crate::wire::{self, HandleInfo};

fn put_le(out: &mut Vec<u8>, value: u128, size: u64) {
    let mut left = value;
    let mut i = 0u64;
    while i < size {
        out.push((left & 0xFF) as u8);
        left >>= 8;
        i = i.saturating_add(1);
    }
}

/// A valid value of a fixed-size type, deterministic and non-zero wherever that is allowed.
fn sample_fixed(ty: &Type, out: &mut Vec<u8>, minor: u16, library: &str) {
    let minor = if type_library(ty).is_some_and(|l| l != library) {
        u16::MAX
    } else {
        minor
    };
    match ty {
        Type::Bool => out.push(1),
        Type::Int(int) => {
            let size = int.size();
            let pattern: u128 = 0x1122_3344_5566_7788u128;
            let shifted = pattern >> (64u64.saturating_sub(size.saturating_mul(8)));
            put_le(out, shifted, size);
        }
        Type::Enum(e) => {
            let member = e
                .members
                .iter()
                .find(|m| m.since.minor <= minor)
                .or(e.members.first());
            let value = member.map_or(0, |m| m.value);
            let mask = if e.repr.size() == 8 {
                u128::from(u64::MAX)
            } else {
                (1u128 << e.repr.size().saturating_mul(8)).wrapping_sub(1)
            };
            put_le(out, value.cast_unsigned() & mask, e.repr.size());
        }
        Type::Bits(b) => put_le(out, u128::from(b.known_mask(minor)), b.repr.size()),
        Type::Struct(s) => sample_struct(s, out, minor),
        Type::Array(elem, count) => {
            let mut i = 0;
            while i < *count {
                sample_fixed(elem, out, minor, library);
                i = i.saturating_add(1);
            }
        }
        Type::Vector(..) | Type::String(_) | Type::Handle(_) => {}
    }
}

fn sample_struct(s: &Struct, out: &mut Vec<u8>, minor: u16) {
    let start = out.len();
    let total = usize::try_from(s.size).unwrap_or(0);
    out.resize(start.saturating_add(total), 0);
    for field in &s.fields {
        let mut bytes = Vec::new();
        sample_fixed(&field.ty, &mut bytes, minor, &s.library);
        let at = start.saturating_add(usize::try_from(field.offset).unwrap_or(0));
        if let Some(region) = out.get_mut(at..at.saturating_add(bytes.len())) {
            region.copy_from_slice(&bytes);
        }
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

fn param_handle(param: &Param) -> Option<&HandleType> {
    match &param.ty {
        Type::Handle(h) => Some(h),
        Type::Vector(elem, _) => match elem.as_ref() {
            Type::Handle(h) => Some(h),
            _ => None,
        },
        _ => None,
    }
}

/// How many elements the sample of a vector or string has.
fn sample_count(bound: u32) -> u32 {
    bound.min(3)
}

/// A valid message of `params` at `minor`, and its handle list.
fn sample_message(
    method: &Method,
    library: &str,
    params: &[Param],
    request: bool,
    minor: u16,
) -> (Vec<u8>, Vec<HandleInfo>) {
    let layout = layout::message_layout(params, minor);
    let inline = usize::try_from(layout.inline_size).unwrap_or(0);
    let mut body = vec![0u8; inline];
    let mut handles = Vec::new();
    let mut variable: Vec<u8> = Vec::new();

    // Handle slots are assigned in decode order: inline first, then the variable part.
    let mut slot = 0u32;
    let info = |handle: &HandleType, slot: &mut u32| -> HandleInfo {
        let current = *slot;
        *slot = slot.saturating_add(1);
        let _ = current;
        let object = if handle.object_value != 0 {
            u32::try_from(handle.object_value).unwrap_or(1)
        } else {
            1
        };
        HandleInfo {
            object,
            rights: u32::try_from(handle.rights_mask).unwrap_or(0),
        }
    };
    for placed in &layout.placed {
        let Some(param) = params.get(placed.param) else {
            continue;
        };
        let at = usize::try_from(placed.offset).unwrap_or(0);
        match placed.kind {
            InlineKind::Fixed => {
                let mut bytes = Vec::new();
                sample_fixed(&param.ty, &mut bytes, minor, library);
                if let Some(region) = body.get_mut(at..at.saturating_add(bytes.len())) {
                    region.copy_from_slice(&bytes);
                }
            }
            InlineKind::Count => {
                let bound = match &param.ty {
                    Type::Vector(_, bound) | Type::String(bound) => *bound,
                    _ => 0,
                };
                let count = sample_count(bound);
                if let Some(region) = body.get_mut(at..at.saturating_add(4)) {
                    region.copy_from_slice(&count.to_le_bytes());
                }
            }
            InlineKind::Slot => {
                if let Some(handle) = param_handle(param) {
                    let current = slot;
                    let entry = info(handle, &mut slot);
                    handles.push(entry);
                    if let Some(region) = body.get_mut(at..at.saturating_add(4)) {
                        region.copy_from_slice(&current.to_le_bytes());
                    }
                }
            }
        }
    }
    for index in &layout.variable {
        let Some(param) = params.get(*index) else {
            continue;
        };
        let mut bytes = Vec::new();
        match &param.ty {
            Type::String(bound) => bytes.extend_from_slice(
                b"abc"
                    .get(..usize::try_from(sample_count(*bound)).unwrap_or(0))
                    .unwrap_or(&[]),
            ),
            Type::Vector(elem, bound) => {
                let count = sample_count(*bound);
                if let Type::Handle(handle) = elem.as_ref() {
                    let mut i = 0;
                    while i < count {
                        let current = slot;
                        let entry = info(handle, &mut slot);
                        handles.push(entry);
                        bytes.extend_from_slice(&current.to_le_bytes());
                        i = i.saturating_add(1);
                    }
                } else {
                    let mut i = 0;
                    while i < count {
                        sample_fixed(elem, &mut bytes, minor, library);
                        i = i.saturating_add(1);
                    }
                }
            }
            _ => {}
        }
        let padded = layout::round_up(bytes.len() as u64, 8);
        bytes.resize(usize::try_from(padded).unwrap_or(0), 0);
        variable.extend_from_slice(&bytes);
    }
    let total = 16usize
        .saturating_add(body.len())
        .saturating_add(variable.len());
    let mut message = Vec::with_capacity(total);
    message.extend_from_slice(&u32::try_from(total).unwrap_or(0).to_le_bytes());
    message.push(1);
    message.push(if request { 1 } else { 2 });
    message.extend_from_slice(&[0, 0]);
    message.extend_from_slice(&method.ordinal.to_le_bytes());
    let txid: u32 = if request && method.oneway { 0 } else { 7 };
    message.extend_from_slice(&txid.to_le_bytes());
    message.extend_from_slice(&body);
    message.extend_from_slice(&variable);
    (message, handles)
}

/// Writes the `size` field back after a change of length, so the checks behind it are reached.
fn reseal(message: &mut [u8]) {
    let total = u32::try_from(message.len()).unwrap_or(0);
    if let Some(region) = message.get_mut(0..4) {
        region.copy_from_slice(&total.to_le_bytes());
    }
}

fn handle_text(index: usize, info: &HandleInfo) -> String {
    format!(
        "{:x}:{:x}:{:x}",
        index.saturating_add(1),
        info.object,
        info.rights
    )
}

fn handles_text(handles: &[HandleInfo]) -> String {
    let parts: Vec<String> = handles
        .iter()
        .enumerate()
        .map(|(i, h)| handle_text(i, h))
        .collect();
    parts.join(",")
}

/// The most text a corpus holds (§14): a definition with huge messages can ask for more, and
/// the tool must stay bounded on any input.
pub const MAX_CORPUS_BYTES: usize = 64 << 20;

/// A corpus writer that keeps each line once, up to [`MAX_CORPUS_BYTES`].
struct Sink {
    text: String,
    seen: HashSet<u64>,
    truncated: bool,
}

impl Sink {
    fn add(&mut self, line: String) {
        // Room is kept for the closing line.
        if self
            .text
            .len()
            .saturating_add(line.len())
            .saturating_add(64)
            > MAX_CORPUS_BYTES
        {
            self.truncated = true;
            return;
        }
        let hash = origin_crypto::Sha256::digest(line.as_bytes());
        let mut first = [0u8; 8];
        first.copy_from_slice(hash.get(..8).unwrap_or(&[0; 8]));
        if self.seen.insert(u64::from_le_bytes(first)) {
            self.text.push_str(&line);
            self.text.push('\n');
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn add_message(
    sink: &mut Sink,
    library: &str,
    interface: &str,
    method: &Method,
    request: bool,
    minor: u16,
    message: &[u8],
    handles: &[HandleInfo],
) {
    let direction = if request { "request" } else { "response" };
    let verdict = wire::check_message(method, library, request, minor, message, handles);
    let expect = match verdict {
        Ok(()) => "ok".to_string(),
        Err(reason) => reason.name().to_string(),
    };
    sink.add(format!(
        "message {interface} {} {direction} minor={minor} handles={} bytes={} expect={expect}",
        method.name,
        handles_text(handles),
        hex(message)
    ));
}

const REPLACEMENTS: [u8; 6] = [0x00, 0x01, 0x02, 0x7F, 0x80, 0xFF];

/// Most bytes of a message whose positions are all mutated (§14); above it, the first and last
/// [`POSITIONS_KEPT`] of them.
const POSITION_LIMIT: usize = 1024;
/// How many positions of each end of a long message are mutated.
const POSITIONS_KEPT: usize = 512;

/// The byte positions mutated in a message of `len` bytes.
fn positions(len: usize) -> impl Iterator<Item = usize> {
    let keep = if len <= POSITION_LIMIT {
        len
    } else {
        POSITIONS_KEPT
    };
    let last = if len <= POSITION_LIMIT {
        len
    } else {
        len.saturating_sub(POSITIONS_KEPT)
    };
    (0..keep).chain(last..len)
}

/// The scalars inside a fixed-size value: `(offset, leaf type)`.
fn leaves<'t>(ty: &'t Type, at: usize, out: &mut Vec<(usize, &'t Type)>) {
    match ty {
        Type::Bool | Type::Int(_) | Type::Enum(_) | Type::Bits(_) => out.push((at, ty)),
        Type::Struct(s) => {
            for field in &s.fields {
                let offset = at.saturating_add(usize::try_from(field.offset).unwrap_or(0));
                leaves(&field.ty, offset, out);
            }
        }
        Type::Array(elem, count) => {
            let size = usize::try_from(layout::fixed_layout(elem).0).unwrap_or(0);
            let mut i = 0usize;
            while (i as u64) < u64::from(*count) {
                leaves(elem, at.saturating_add(i.saturating_mul(size)), out);
                i = i.saturating_add(1);
            }
        }
        Type::Vector(..) | Type::String(_) | Type::Handle(_) => {}
    }
}

/// Field-level replacements the single-byte pass cannot reach (§14): every declared member of an
/// enum, whatever minor it arrived in; every bit a bits type declares; and all-zero for a
/// `@nonzero` field.
fn leaf_cases(ty: &Type, nonzero: bool) -> Vec<(Vec<u8>, usize)> {
    fn le(value: u128, size: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        put_le(&mut bytes, value, size);
        bytes
    }
    let mut out = Vec::new();
    match ty {
        Type::Enum(e) => {
            let size = e.repr.size();
            let mask = if size == 16 {
                u128::MAX
            } else {
                (1u128 << size.saturating_mul(8)).wrapping_sub(1)
            };
            for member in &e.members {
                out.push((le(member.value.cast_unsigned() & mask, size), 0));
            }
        }
        Type::Bits(b) => {
            let size = b.repr.size();
            let width = size.saturating_mul(8);
            let mut highest = 0u32;
            for member in &b.members {
                let bit = u32::try_from(member.value).unwrap_or(0);
                out.push((le(1u128 << bit, size), 0));
                highest = highest.max(bit);
            }
            // An undeclared bit, if one exists above the highest declared.
            let next = u64::from(highest).saturating_add(1);
            if next < width {
                out.push((le(1u128 << next, size), 0));
            }
            if nonzero {
                out.push((le(0, size), 0));
            }
        }
        Type::Int(int) if nonzero => out.push((le(0, int.size()), 0)),
        _ => {}
    }
    out
}

/// Replaces every field of a sample message with each of [`leaf_cases`].
#[allow(clippy::too_many_arguments)]
fn add_leaf_cases(
    sink: &mut Sink,
    library: &str,
    interface: &str,
    method: &Method,
    params: &[Param],
    request: bool,
    minor: u16,
    message: &[u8],
    handles: &[HandleInfo],
) {
    let layout = layout::message_layout(params, minor);
    let mut sites: Vec<(usize, &Type, bool)> = Vec::new();
    for placed in &layout.placed {
        let Some(param) = params.get(placed.param) else {
            continue;
        };
        if placed.kind == InlineKind::Fixed {
            let mut found = Vec::new();
            leaves(
                &param.ty,
                16usize.saturating_add(usize::try_from(placed.offset).unwrap_or(0)),
                &mut found,
            );
            for (at, ty) in found {
                sites.push((at, ty, param.nonzero));
            }
        }
    }
    // The elements of each vector, at the positions the sample put them.
    let mut at = 16usize.saturating_add(usize::try_from(layout.inline_size).unwrap_or(0));
    for index in &layout.variable {
        let Some(param) = params.get(*index) else {
            continue;
        };
        let count = match &param.ty {
            Type::Vector(_, bound) | Type::String(bound) => sample_count(*bound),
            _ => 0,
        };
        let element = usize::try_from(layout::element_size(&param.ty)).unwrap_or(0);
        if let Type::Vector(elem, _) = &param.ty
            && !matches!(elem.as_ref(), Type::Handle(_))
        {
            let mut i = 0usize;
            while (i as u64) < u64::from(count) {
                let mut found = Vec::new();
                leaves(
                    elem,
                    at.saturating_add(i.saturating_mul(element)),
                    &mut found,
                );
                for (offset, ty) in found {
                    sites.push((offset, ty, false));
                }
                i = i.saturating_add(1);
            }
        }
        let bytes = u64::from(count).saturating_mul(element as u64);
        at = at.saturating_add(usize::try_from(layout::round_up(bytes, 8)).unwrap_or(0));
    }
    for (offset, ty, nonzero) in sites {
        for (bytes, _) in leaf_cases(ty, nonzero) {
            let mut mutated = message.to_vec();
            if let Some(region) = mutated.get_mut(offset..offset.saturating_add(bytes.len())) {
                region.copy_from_slice(&bytes);
            }
            add_message(
                sink, library, interface, method, request, minor, &mutated, handles,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn message_cases(
    sink: &mut Sink,
    library: &str,
    interface: &str,
    method: &Method,
    params: &[Param],
    request: bool,
    minor: u16,
) {
    let (message, handles) = sample_message(method, library, params, request, minor);
    add_message(
        sink, library, interface, method, request, minor, &message, &handles,
    );
    // Every single byte replaced (the ends of a long message).
    for index in positions(message.len()) {
        let original = message.get(index).copied().unwrap_or(0);
        for replacement in REPLACEMENTS.iter().copied().chain([original ^ 1]) {
            if replacement == original {
                continue;
            }
            let mut mutated = message.clone();
            if let Some(slot) = mutated.get_mut(index) {
                *slot = replacement;
            }
            add_message(
                sink, library, interface, method, request, minor, &mutated, &handles,
            );
        }
    }
    add_leaf_cases(
        sink, library, interface, method, params, request, minor, &message, &handles,
    );
    // Every truncation, and extensions, with the header's size re-sealed so the checks behind
    // it are reached.
    for length in positions(message.len()) {
        let mut shorter = message
            .get(..length)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        reseal(&mut shorter);
        add_message(
            sink, library, interface, method, request, minor, &shorter, &handles,
        );
    }
    for extra in [8usize, 16, 1, 7] {
        let mut longer = message.clone();
        longer.resize(message.len().saturating_add(extra), 0);
        reseal(&mut longer);
        add_message(
            sink, library, interface, method, request, minor, &longer, &handles,
        );
    }
    // Handle lists: one short, one long; each handle with the wrong type and missing a right.
    if let Some((_, rest)) = handles.split_last() {
        add_message(
            sink, library, interface, method, request, minor, &message, rest,
        );
    }
    let mut extended = handles.clone();
    extended.push(HandleInfo {
        object: 1,
        rights: u32::MAX,
    });
    add_message(
        sink, library, interface, method, request, minor, &message, &extended,
    );
    for index in 0..handles.len() {
        let mut wrong = handles.clone();
        if let Some(entry) = wrong.get_mut(index) {
            entry.object = entry.object.wrapping_add(1);
        }
        add_message(
            sink, library, interface, method, request, minor, &message, &wrong,
        );
        let mut weak = handles.clone();
        if let Some(entry) = weak.get_mut(index) {
            entry.rights = 0;
        }
        add_message(
            sink, library, interface, method, request, minor, &message, &weak,
        );
    }
}

const EDGE_VALUES: [u64; 12] = [
    0,
    1,
    2,
    0x7F,
    0x80,
    0xFF,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
    0x1_0000_0000,
    0x8000_0000_0000_0000,
    u64::MAX,
];

fn add_call(sink: &mut Sink, interface: &str, method: &Method, holds: bool, args: [u64; 6]) {
    let expect = match wire::check_call(method, holds, args) {
        Ok(()) => "ok".to_string(),
        Err(name) => name,
    };
    let list: Vec<String> = args.iter().map(|a| format!("{a:x}")).collect();
    sink.add(format!(
        "syscall {interface} {} {} holds={} args={} expect={expect}",
        method.name,
        method.ordinal,
        u8::from(holds),
        list.join(",")
    ));
}

fn syscall_cases(sink: &mut Sink, interface: &str, method: &Method) {
    // Good values first, then each argument register at every edge value.
    let good: [u64; 6] = [1, 1, 1, 1, 1, 1];
    for holds in [false, true] {
        add_call(sink, interface, method, holds, good);
        for position in 0..6 {
            for value in EDGE_VALUES {
                let mut args = good;
                if let Some(slot) = args.get_mut(position) {
                    *slot = value;
                }
                add_call(sink, interface, method, holds, args);
            }
        }
        // Pairs of adjacent registers at their extremes.
        for position in 0..5usize {
            for (a, b) in [
                (u64::MAX, u64::MAX),
                (0, 0),
                (0x1_0000_0000, 1),
                (1, 0x1_0000_0000),
            ] {
                let mut args = good;
                if let Some(slot) = args.get_mut(position) {
                    *slot = a;
                }
                if let Some(slot) = args.get_mut(position.saturating_add(1)) {
                    *slot = b;
                }
                add_call(sink, interface, method, holds, args);
            }
        }
    }
}

/// The minors a method's corpus covers: each one a layout or a type changes at, at most
/// [`MAX_MINORS`] of them (`version 1.65535` must not multiply the corpus by 65,536).
fn interesting_minors(library: &Library, method: &Method) -> Vec<u16> {
    let mut minors: Vec<u16> = vec![method.since.minor, library.version.minor];
    minors.extend(
        method
            .params
            .iter()
            .chain(&method.returns)
            .map(|p| p.since.minor),
    );
    minors.extend(
        library
            .enums
            .iter()
            .flat_map(|e| e.members.iter().map(|m| m.since.minor)),
    );
    minors.extend(
        library
            .bits
            .iter()
            .flat_map(|b| b.members.iter().map(|m| m.since.minor)),
    );
    minors.retain(|m| *m >= method.since.minor && *m <= library.version.minor);
    minors.sort_unstable();
    minors.dedup();
    if minors.len() > MAX_MINORS {
        let keep = MAX_MINORS / 2;
        let mut bounded: Vec<u16> = minors.iter().take(keep).copied().collect();
        bounded.extend(minors.iter().rev().take(keep).rev().copied());
        bounded.sort_unstable();
        bounded.dedup();
        minors = bounded;
    }
    minors
}

/// At most this many minors per method (§14).
const MAX_MINORS: usize = 8;

/// The corpus of one library.
pub fn corpus_for(library: &Library) -> String {
    let mut sink = Sink {
        text: format!("# oidl-corpus 1 {}\n", library.name),
        seen: HashSet::new(),
        truncated: false,
    };
    for interface in &library.interfaces {
        for method in &interface.methods {
            match library.transport {
                Transport::Syscall => {
                    syscall_cases(&mut sink, &interface.name, method);
                }
                Transport::Channel => {
                    for minor in interesting_minors(library, method) {
                        message_cases(
                            &mut sink,
                            &library.name,
                            &interface.name,
                            method,
                            &method.params,
                            true,
                            minor,
                        );
                        if !method.oneway {
                            message_cases(
                                &mut sink,
                                &library.name,
                                &interface.name,
                                method,
                                &method.returns,
                                false,
                                minor,
                            );
                        }
                    }
                }
            }
        }
    }
    let _ = writeln!(
        sink.text,
        "# end{}",
        if sink.truncated {
            " (truncated at the size limit)"
        } else {
            ""
        }
    );
    sink.text
}
