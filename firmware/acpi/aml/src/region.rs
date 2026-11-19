//! Field units: reading and writing operation regions through the host (ACPI 6.6 §5.5.2.4,
//! §19.6.7, §19.6.48, §19.6.64, §19.3.5.7, Tables 5.221 and 19.34).
//!
//! - **Byte-addressed spaces** (SystemMemory, SystemIO, PCI_Config, EmbeddedControl, SystemCMOS,
//!   PciBarTarget, OEM): a field is read and written in naturally aligned units of its access
//!   width, each unit one host access inside the region. A unit the field covers only in part is
//!   written by the update rule: Preserve reads it first, WriteAsOnes and WriteAsZeros fill the
//!   rest. SystemCMOS takes bytes only, and so does an EmbeddedControl field of 8 bits or fewer;
//!   a wider EmbeddedControl field is one host transfer of the bytes it spans, up to 255 (G5,
//!   TASKS 64's ledger §3; step B5, B5's contract Q6). `AnyAcc`, and `BufferAcc` in these spaces,
//!   is byte access (G5; step B5): every unit lies within the field's bytes, and no unit reaches
//!   past a region's end that the field does not.
//! - **Index fields** write the unit's byte offset to the index field, then read or write the data
//!   field; **bank fields** write the bank value to the bank field first. A chain of fields is at
//!   most [`FIELD_DEPTH`] deep.
//! - **Buffer-protocol spaces** (SMBus, IPMI, GenericSerialBus, PlatformRtMechanism, FFixedHW):
//!   one host transfer of the space's data buffer, with the field's access attribute and its
//!   `Connection`; the result is the buffer the host filled.
//! - **GeneralPurposeIO and PCC**: one host transfer of the field's bytes.
//! - **PCI_Config regions** name their function: the `_ADR` of the nearest `Device` above the
//!   region, the segment and bus of the PCI root bridge above that (`_SEG`, `_BBN`), and a
//!   bridge's secondary bus for each device with an `_ADR` between, found before the first access
//!   by frames on the engine's stack (`pci.rs`) and kept.
//! - A field with the `Lock` rule takes the Global Lock around its access.
//!
//! Whether the region may be touched at all is the host's decision (ADR-0019 A2; `host.rs`).

use alloc::vec::Vec;

use crate::Interpreter;
use crate::error::{Error, ErrorKind, Limit, Result, err};
use crate::host::{BufferAccess, Host, RegionAccess, Space, Trace};
use crate::mem;
use crate::name::Seg;
use crate::object::{Connection, FieldKind, FieldUnit, NodeId, Object, Region, Val};
use crate::store::{get_bits, set_bits};

/// The deepest chain of field units one access goes through.
pub(crate) const FIELD_DEPTH: u32 = 8;

/// How a field's space is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    /// In units, with `read` and `write`.
    Units { byte_only: bool },
    /// One `transfer` of a protocol data buffer of this many bytes (0: the field's own size).
    Protocol(usize),
    /// One `transfer` of the field's own bytes, read or written as a value.
    Pins,
}

fn reach(space: u8) -> Reach {
    match space {
        3 | 5 => Reach::Units { byte_only: true },
        // SMBus: status, length and 32 bytes; IPMI: status, length and 64; GenericSerialBus:
        // status, length and up to 255.
        4 => Reach::Protocol(34),
        7 => Reach::Protocol(66),
        9 => Reach::Protocol(257),
        0x0B | 0x7F => Reach::Protocol(0),
        8 | 0x0A => Reach::Pins,
        _ => Reach::Units { byte_only: false },
    }
}

/// The planted fault `EcBytewise`: an EmbeddedControl field wider than a byte read and written a
/// byte at a time, as before step B5.
fn ec_bytewise() -> bool {
    #[cfg(test)]
    let planted = crate::tests::planted(crate::tests::Fault::EcBytewise);
    #[cfg(not(test))]
    let planted = false;
    planted
}

fn ones(bits: u64) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        1u64.checked_shl(u32::try_from(bits).unwrap_or(64))
            .map_or(u64::MAX, |v| v.saturating_sub(1))
    }
}

/// The byte width of a field's units.
fn width(f: &FieldUnit, byte_only: bool) -> u64 {
    if byte_only {
        return 1;
    }
    #[cfg(test)]
    let widest = crate::tests::planted(crate::tests::Fault::WidestAnyAcc);
    #[cfg(not(test))]
    let widest = false;
    match f.access {
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        // `AnyAcc` and `BufferAcc`: byte access (G5, TASKS 64's ledger §3; step B5).
        _ if !widest => 1,
        // The planted fault `WidestAnyAcc`: the rule before step B5, the narrowest naturally
        // aligned width that holds the whole field.
        _ => {
            let last = f.bit_offset.saturating_add(f.bit_len.saturating_sub(1));
            [1u64, 2, 4, 8]
                .into_iter()
                .find(|w| {
                    let bits = w.saturating_mul(8);
                    f.bit_offset.checked_div(bits) == last.checked_div(bits)
                })
                .unwrap_or(1)
        }
    }
}

/// Where a field's units are.
#[derive(Clone, Copy, Debug)]
enum Path {
    Region(Region),
    Index { index: NodeId, data: NodeId },
}

impl<H: Host> Interpreter<H> {
    fn unit_of(&self, node: NodeId) -> Result<FieldUnit> {
        match self.heap.get(self.ns.obj(node)?)? {
            Object::FieldUnit(f) => Ok(*f),
            _ => err(ErrorKind::WrongObject),
        }
    }

    fn region_of(&mut self, node: NodeId) -> Result<Region> {
        let obj = self.ns.obj(node)?;
        let r = match self.heap.get(obj)? {
            Object::Region(r) => *r,
            _ => return err(ErrorKind::WrongObject),
        };
        // Spaces 0x0C to 0x7E are reserved (Table 5.221): nothing is defined to be there, so no
        // access to one reaches the host (the review of B2, B2-11). 0x7F (FFixedHW) and the OEM
        // spaces from 0x80 are the host's to judge.
        if (0x0C..=0x7E).contains(&r.space) {
            return err(ErrorKind::Range);
        }
        // Its function is found before the access starts (`pci.rs`); one not yet found here is
        // asked for the same way.
        if r.space == Space::PCI_CONFIG.0 && r.pci.is_none() {
            return Err(self.need_address(node));
        }
        Ok(r)
    }

    /// A field unit's value (§19.3.5.7: an integer if it fits one, else a buffer).
    pub(crate) fn read_field(&mut self, node: NodeId) -> Result<Val> {
        if let Some(r) = self.unresolved(node)? {
            return Err(self.need_address(r));
        }
        self.field_at(node, None, 0)
    }

    /// Writes `v` to a field unit. For a buffer-protocol field, the buffer the host returned,
    /// which is `Store`'s result (§19.6.132).
    pub(crate) fn write_field(&mut self, node: NodeId, v: Val) -> Result<Option<Val>> {
        if let Some(r) = self.unresolved(node)? {
            return Err(self.need_address(r));
        }
        self.write_value(node, v, 0)
    }

    fn write_value(&mut self, node: NodeId, v: Val, depth: u32) -> Result<Option<Val>> {
        let f = self.unit_of(node)?;
        let bytes = usize::try_from(f.bit_len.div_ceil(8)).map_err(|_| ErrorKind::Range)?;
        // A string is written as a buffer of its bytes, without its NUL: into a field it fits,
        // one write, zero-extended; longer, in field-sized pieces as a buffer is (G15's string
        // half, as the coordinating session ruled it again on 2026-11-19, step B5's point 1;
        // ACPICA's single write cut to the field, TASKS 64's ledger §3, considered and not
        // adopted).
        let string = match v {
            Val::Obj(r) => match self.heap.get(r)? {
                Object::String(s) => Some(mem::copy_bytes(s, &mut self.budget)?),
                _ => None,
            },
            _ => None,
        };
        #[cfg(test)]
        if let Some(chars) = &string
            && let Some(out) = self.planted_string_rule(node, chars, bytes, depth)
        {
            if let Some(chars) = string {
                mem::release(chars, &mut self.budget);
            }
            return out;
        }
        let data = match (string, v) {
            (Some(chars), _) => chars,
            (None, Val::Int(i)) => mem::copy_bytes(&i.to_le_bytes(), &mut self.budget)?,
            (None, other) => self.buffer_of(other)?,
        };
        let protocol = match f.kind {
            FieldKind::Region(rn) | FieldKind::Bank { region: rn, .. } => {
                matches!(reach(self.region_space(rn)?), Reach::Protocol(_))
            }
            FieldKind::Index { .. } => false,
        };
        let out = if protocol || data.len() <= bytes || matches!(v, Val::Int(_)) {
            // One write: an integer truncated, a buffer within the field zero-extended.
            self.field_at(node, Some(&data), depth)
                .map(|r| protocol.then_some(r))
        } else if f.bit_len == 0 {
            // A field of no bits holds nothing: there are no pieces to write. Before, the loop
            // below advanced by the field's width, 0, for ever (found by the soak of 2026-11-14).
            Ok(None)
        } else {
            // Data longer than the field is written in field-sized pieces, lower first, the last
            // zero-extended (Table 19.7's Buffer to Field Unit row), each a region access counted
            // as work, so a long buffer into a narrow field ends at a limit. The host is told
            // first: ACPICA writes one piece, cut.
            let mut r = Ok(None);
            let bits = u64::try_from(data.len()).unwrap_or(0).saturating_mul(8);
            self.tell_pieces(node, bits.div_ceil(f.bit_len));
            let mut at: u64 = 0;
            let mut piece = mem::zeroed(bytes, &mut self.budget)?;
            while at < bits && r.is_ok() {
                piece.fill(0);
                let n = f.bit_len.min(bits.saturating_sub(at));
                r = self
                    .charge(1)
                    .and_then(|()| get_bits(&data, at, n, &mut piece))
                    .and_then(|()| self.field_at(node, Some(&piece), depth))
                    .map(|_| None);
                at = at.saturating_add(f.bit_len);
            }
            mem::release(piece, &mut self.budget);
            r
        };
        mem::release(data, &mut self.budget);
        out
    }

    /// The planted faults on a string's store: `StringByCharacter`, the rule before step B5, a
    /// character at a time; `StringCut`, step B5's first rule for G15, the bytes cut to the field
    /// in one write. `None` when neither is planted.
    #[cfg(test)]
    fn planted_string_rule(
        &mut self,
        node: NodeId,
        chars: &[u8],
        bytes: usize,
        depth: u32,
    ) -> Option<Result<Option<Val>>> {
        use crate::tests::{Fault, planted};
        if planted(Fault::StringCut) {
            let cut = chars.get(..bytes.min(chars.len())).unwrap_or(&[]);
            return Some(self.field_at(node, Some(cut), depth).map(|_| None));
        }
        if !planted(Fault::StringByCharacter) {
            return None;
        }
        for &c in chars {
            // Each character is a write, work counted whatever the field's width.
            if let Err(e) = self
                .charge(1)
                .and_then(|()| self.field_at(node, Some(&[c]), depth))
            {
                return Some(Err(e));
            }
        }
        Some(Ok(None))
    }

    fn region_space(&self, node: NodeId) -> Result<u8> {
        match self.heap.get(self.ns.obj(node)?)? {
            Object::Region(r) => Ok(r.space),
            _ => err(ErrorKind::WrongObject),
        }
    }

    /// Reads (`write` is `None`) or writes the field unit `node`; returns what was read, or for
    /// a buffer-protocol write the buffer the host returned.
    fn field_at(&mut self, node: NodeId, write: Option<&[u8]>, depth: u32) -> Result<Val> {
        if depth > FIELD_DEPTH {
            return err(ErrorKind::Range);
        }
        let f = self.unit_of(node)?;
        if f.lock {
            self.lock_global()?;
        }
        let r = self.field_locked(&f, write, depth);
        if f.lock {
            let u = self.unlock_global();
            if r.is_ok() {
                u?;
            }
        }
        r
    }

    fn field_locked(&mut self, f: &FieldUnit, write: Option<&[u8]>, depth: u32) -> Result<Val> {
        let next = depth.saturating_add(1);
        let path = match f.kind {
            FieldKind::Region(rn) => Path::Region(self.region_of(rn)?),
            FieldKind::Bank {
                region,
                bank,
                value,
            } => {
                let v = mem::copy_bytes(&value.to_le_bytes(), &mut self.budget)?;
                let r = self.field_at(bank, Some(&v), next);
                mem::release(v, &mut self.budget);
                r?;
                Path::Region(self.region_of(region)?)
            }
            FieldKind::Index { index, data } => Path::Index { index, data },
        };
        if let Path::Region(region) = path {
            match reach(region.space) {
                Reach::Protocol(size) => return self.protocol(f, &region, write, size),
                Reach::Pins => return self.pins(f, &region, write),
                Reach::Units { .. } if region.space == 3 && f.bit_len > 8 && !ec_bytewise() => {
                    return self.ec_wide(f, &region, write);
                }
                Reach::Units { .. } => {}
            }
        }
        let byte_only = match path {
            Path::Region(r) => matches!(reach(r.space), Reach::Units { byte_only: true }),
            Path::Index { .. } => false,
        };
        let w = width(f, byte_only);
        let ubits = w.saturating_mul(8);
        let bytes = usize::try_from(f.bit_len.div_ceil(8)).map_err(|_| ErrorKind::Range)?;
        let end = f
            .bit_offset
            .checked_add(f.bit_len)
            .ok_or(ErrorKind::Range)?;
        if f.bit_len == 0 {
            return Ok(Val::Int(0));
        }
        let first = f.bit_offset.checked_div(ubits).ok_or(ErrorKind::Range)?;
        let last = end
            .saturating_sub(1)
            .checked_div(ubits)
            .ok_or(ErrorKind::Range)?;
        // Every unit inside the region before the first is touched: a write that straddles the
        // region's end is refused whole, never written in part (the review of B2, B2-8).
        if let Path::Region(r) = path {
            let span = last
                .checked_add(1)
                .and_then(|n| n.checked_mul(w))
                .ok_or(ErrorKind::Range)?;
            if span > r.length {
                return err(ErrorKind::Range);
            }
        }
        let region_path = self.path_of_region(&path)?;
        let mut buf = mem::zeroed(bytes, &mut self.budget)?;
        if let Some(src) = write {
            for (d, s) in buf.iter_mut().zip(src) {
                *d = *s;
            }
        }
        let mut result = Ok(());
        let mut u = first;
        while u <= last && result.is_ok() {
            // Each unit is a host access: work, counted, with the clock read as it mounts.
            result = self.charge(1);
            if result.is_err() {
                break;
            }
            result = self.one_unit(
                f,
                &path,
                &region_path,
                u,
                w,
                write.is_some(),
                &mut buf,
                next,
            );
            u = u.saturating_add(1);
        }
        mem::release(region_path, &mut self.budget);
        if let Err(e) = result {
            mem::release(buf, &mut self.budget);
            return Err(e);
        }
        if write.is_some() {
            mem::release(buf, &mut self.budget);
            return Ok(Val::None);
        }
        self.field_value(f.bit_len, buf)
    }

    /// One unit of a field: read into `buf`, or written from it.
    #[allow(clippy::too_many_arguments)]
    fn one_unit(
        &mut self,
        f: &FieldUnit,
        path: &Path,
        region_path: &[Seg],
        u: u64,
        w: u64,
        write: bool,
        buf: &mut [u8],
        depth: u32,
    ) -> Result<()> {
        let ubits = w.saturating_mul(8);
        let ustart = u.saturating_mul(ubits);
        let lo = f.bit_offset.max(ustart);
        let hi = f
            .bit_offset
            .saturating_add(f.bit_len)
            .min(ustart.saturating_add(ubits));
        let n = hi.saturating_sub(lo);
        let shift = lo.saturating_sub(ustart);
        let byte_off = u.saturating_mul(w);
        if !write {
            let v = self.unit_read(path, region_path, byte_off, w, depth)?;
            let mut tmp = [0u8; 8];
            get_bits(&v.to_le_bytes(), shift, n, &mut tmp)?;
            return set_bits(buf, lo.saturating_sub(f.bit_offset), n, &tmp);
        }
        let mut bits = [0u8; 8];
        get_bits(buf, lo.saturating_sub(f.bit_offset), n, &mut bits)?;
        let field = u64::from_le_bytes(bits)
            .checked_shl(u32::try_from(shift).unwrap_or(64))
            .unwrap_or(0);
        let mask = ones(n)
            .checked_shl(u32::try_from(shift).unwrap_or(64))
            .unwrap_or(0);
        let whole = ones(ubits);
        let base = if mask == whole {
            0
        } else {
            match f.update {
                0 => self.unit_read(path, region_path, byte_off, w, depth)?,
                1 => whole,
                _ => 0,
            }
        };
        self.unit_write(
            path,
            region_path,
            byte_off,
            w,
            (base & !mask) | (field & mask),
            depth,
        )
    }

    fn unit_read(
        &mut self,
        path: &Path,
        region_path: &[Seg],
        byte_off: u64,
        w: u64,
        depth: u32,
    ) -> Result<u64> {
        match *path {
            Path::Region(r) => {
                let left = self.time_for_host()?;
                let access = self.access(&r, region_path, byte_off, w, left)?;
                let got = self.host.read(&access);
                self.trace(&Trace::Read(&access, got));
                got.map_err(|cause| {
                    Error::from(ErrorKind::Region {
                        space: r.space,
                        address: access.address,
                        cause,
                    })
                })
            }
            Path::Index { index, data } => {
                let off = byte_off.to_le_bytes();
                self.field_at(index, Some(&off), depth)?;
                let v = self.field_at(data, None, depth)?;
                let x = match v {
                    Val::Int(x) => x,
                    other => self.int_of(other)?,
                };
                Ok(x & ones(w.saturating_mul(8)))
            }
        }
    }

    fn unit_write(
        &mut self,
        path: &Path,
        region_path: &[Seg],
        byte_off: u64,
        w: u64,
        value: u64,
        depth: u32,
    ) -> Result<()> {
        match *path {
            Path::Region(r) => {
                let left = self.time_for_host()?;
                let access = self.access(&r, region_path, byte_off, w, left)?;
                let got = self.host.write(&access, value);
                self.trace(&Trace::Write(&access, value, got));
                got.map_err(|cause| {
                    Error::from(ErrorKind::Region {
                        space: r.space,
                        address: access.address,
                        cause,
                    })
                })
            }
            Path::Index { index, data } => {
                let off = byte_off.to_le_bytes();
                self.field_at(index, Some(&off), depth)?;
                let v = value.to_le_bytes();
                self.field_at(data, Some(&v), depth).map(|_| ())
            }
        }
    }

    /// The host's view of one unit: inside the region, or refused.
    fn access<'a>(
        &self,
        r: &Region,
        region_path: &'a [Seg],
        byte_off: u64,
        w: u64,
        time_left_100ns: u64,
    ) -> Result<RegionAccess<'a>> {
        let end = byte_off.checked_add(w).ok_or(ErrorKind::Range)?;
        if end > r.length {
            return err(ErrorKind::Range);
        }
        let address = r.offset.checked_add(byte_off).ok_or(ErrorKind::Range)?;
        Ok(RegionAccess {
            space: Space(r.space),
            address,
            width: u8::try_from(w.saturating_mul(8)).unwrap_or(8),
            pci: r.pci,
            region: region_path,
            time_left_100ns,
        })
    }

    /// The whole of a SystemMemory region, read through the host for `Load` (§19.6.77; B4's
    /// contract, Q5, as ruled with B2-3's and B2-8's rules): only within the region as declared,
    /// its span checked whole before the first access (it must lie in the address space, and the
    /// memory limit must hold it), then in the widest naturally aligned accesses, each a unit of
    /// work.
    pub(crate) fn region_bytes(&mut self, r: &Region) -> Result<Vec<u8>> {
        r.offset.checked_add(r.length).ok_or(ErrorKind::Range)?;
        let len = usize::try_from(r.length).map_err(|_| ErrorKind::Limit(Limit::Memory))?;
        let mut out = mem::with_capacity(len, &mut self.budget)?;
        let path = Path::Region(*r);
        let region_path = self.path_of_region(&path)?;
        let mut off: u64 = 0;
        let read = loop {
            if off >= r.length {
                break Ok(());
            }
            let address = r.offset.saturating_add(off);
            let left = r.length.saturating_sub(off);
            let w = [8u64, 4, 2, 1]
                .into_iter()
                .find(|&w| w <= left && address.is_multiple_of(w))
                .unwrap_or(1);
            let step = self.charge(1).and_then(|()| {
                let x = self.unit_read(&path, &region_path, off, w, 0)?;
                let bytes = x.to_le_bytes();
                let part = bytes.get(..usize::try_from(w).unwrap_or(1)).unwrap_or(&[]);
                mem::extend_bytes(&mut out, part, &mut self.budget)
            });
            if let Err(e) = step {
                break Err(e);
            }
            off = off.saturating_add(w);
        };
        mem::release(region_path, &mut self.budget);
        match read {
            Ok(()) => Ok(out),
            Err(e) => {
                mem::release(out, &mut self.budget);
                Err(e)
            }
        }
    }

    fn path_of_region(&mut self, path: &Path) -> Result<Vec<Seg>> {
        let mut out = Vec::new();
        if let Path::Region(r) = path {
            self.ns.path(r.node, &mut out, &mut self.budget)?;
        }
        Ok(out)
    }

    /// The value of `buf`, a field of `bits`: an integer if it fits one, else a buffer.
    fn field_value(&mut self, bits: u64, buf: Vec<u8>) -> Result<Val> {
        if bits <= u64::from(self.width.bits()) {
            let mut x: u64 = 0;
            for (k, &b) in buf.iter().take(8).enumerate() {
                x |= u64::from(b) << (k.saturating_mul(8) & 63);
            }
            mem::release(buf, &mut self.budget);
            Ok(Val::Int(x & self.width.mask()))
        } else {
            self.new_buffer(buf)
        }
    }

    /// The bytes of a field's `Connection`, if it has one.
    fn connection_bytes(&mut self, f: &FieldUnit) -> Result<Option<Vec<u8>>> {
        let r = match f.connection {
            Connection::None => return Ok(None),
            Connection::Buffer(r) => r,
            Connection::Name(e) => {
                let node = self.resolve_elem(e)?;
                self.ns.obj(node)?
            }
        };
        match self.heap.get(r)? {
            Object::Buffer(b) => Ok(Some(mem::copy_bytes(b, &mut self.budget)?)),
            _ => err(ErrorKind::Type),
        }
    }

    /// One transfer of a protocol buffer (SMBus, IPMI, GenericSerialBus, PRM, FFH).
    fn protocol(
        &mut self,
        f: &FieldUnit,
        r: &Region,
        write: Option<&[u8]>,
        size: usize,
    ) -> Result<Val> {
        // The field must start inside its region: for SMBus and GenericSerialBus the region's
        // length is the command space (§5.5.2.4.7; the review of B2, B2-3).
        if f.bit_offset / 8 >= r.length {
            return err(ErrorKind::Range);
        }
        let own = usize::try_from(f.bit_len.div_ceil(8)).map_err(|_| ErrorKind::Range)?;
        let size = if size == 0 { own } else { size };
        let mut data = mem::zeroed(size, &mut self.budget)?;
        if let Some(src) = write {
            for (d, s) in data.iter_mut().zip(src) {
                *d = *s;
            }
        }
        let conn = self.connection_bytes(f)?;
        let region_path = self.path_of_region(&Path::Region(*r))?;
        let address = r
            .offset
            .checked_add(f.bit_offset / 8)
            .ok_or(ErrorKind::Range)?;
        let got = match self.time_for_host() {
            Ok(time_left_100ns) => {
                let access = BufferAccess {
                    space: Space(r.space),
                    address,
                    write: write.is_some(),
                    attribute: f.attrib,
                    length: f.access_len,
                    connection: conn.as_deref(),
                    bit_offset: f.conn_bit_offset,
                    bit_length: f.bit_len,
                    region: &region_path,
                    time_left_100ns,
                };
                let got = self.host.transfer(&access, &mut data);
                self.trace(&Trace::Transfer(&access, got));
                got.map_err(|cause| {
                    Error::from(ErrorKind::Region {
                        space: r.space,
                        address,
                        cause,
                    })
                })
            }
            Err(e) => Err(e),
        };
        mem::release(region_path, &mut self.budget);
        if let Some(c) = conn {
            mem::release(c, &mut self.budget);
        }
        let filled = match got {
            Ok(n) => n.min(size),
            Err(e) => {
                mem::release(data, &mut self.budget);
                return Err(e);
            }
        };
        data.truncate(filled);
        self.new_buffer(data)
    }

    /// One transfer of a field's own bytes (GeneralPurposeIO, PCC).
    /// An EmbeddedControl field wider than 8 bits: one host transfer of the bytes it spans, up
    /// to 255, all inside the region (G5, TASKS 64's ledger §3; step B5, B5's contract Q6). A
    /// write that does not fill whole bytes merges by the update rule: Preserve reads the bytes
    /// first, in one transfer of its own.
    fn ec_wide(&mut self, f: &FieldUnit, r: &Region, write: Option<&[u8]>) -> Result<Val> {
        let shift = f.bit_offset % 8;
        let span = shift
            .checked_add(f.bit_len)
            .ok_or(ErrorKind::Range)?
            .div_ceil(8);
        let first = f.bit_offset / 8;
        if span > 255 || first.checked_add(span).is_none_or(|e| e > r.length) {
            return err(ErrorKind::Range);
        }
        let address = r.offset.checked_add(first).ok_or(ErrorKind::Range)?;
        let n = usize::try_from(span).map_err(|_| ErrorKind::Range)?;
        let mut data = mem::zeroed(n, &mut self.budget)?;
        let out = match write {
            None => self.ec_read(f, r, address, shift, &mut data),
            Some(src) => self.ec_write(f, r, address, shift, src, &mut data),
        };
        mem::release(data, &mut self.budget);
        out
    }

    fn ec_read(
        &mut self,
        f: &FieldUnit,
        r: &Region,
        address: u64,
        shift: u64,
        data: &mut [u8],
    ) -> Result<Val> {
        self.ec_transfer(f, r, address, false, data)?;
        let bytes = usize::try_from(f.bit_len.div_ceil(8)).map_err(|_| ErrorKind::Range)?;
        let mut value = mem::zeroed(bytes, &mut self.budget)?;
        if let Err(e) = get_bits(data, shift, f.bit_len, &mut value) {
            mem::release(value, &mut self.budget);
            return Err(e);
        }
        self.field_value(f.bit_len, value)
    }

    fn ec_write(
        &mut self,
        f: &FieldUnit,
        r: &Region,
        address: u64,
        shift: u64,
        src: &[u8],
        data: &mut [u8],
    ) -> Result<Val> {
        if shift != 0 || !f.bit_len.is_multiple_of(8) {
            match f.update {
                0 => self.ec_transfer(f, r, address, false, data)?,
                1 => data.fill(0xFF),
                _ => {}
            }
        }
        // The value's bits, from bit 0, zero-extended to the field.
        let bytes = usize::try_from(f.bit_len.div_ceil(8)).map_err(|_| ErrorKind::Range)?;
        let mut value = mem::zeroed(bytes, &mut self.budget)?;
        for (d, s) in value.iter_mut().zip(src) {
            *d = *s;
        }
        let r0 = set_bits(data, shift, f.bit_len, &value);
        mem::release(value, &mut self.budget);
        r0?;
        self.ec_transfer(f, r, address, true, data)?;
        Ok(Val::None)
    }

    /// One host transfer of an EmbeddedControl field's bytes (`ec_wide`).
    fn ec_transfer(
        &mut self,
        f: &FieldUnit,
        r: &Region,
        address: u64,
        write: bool,
        data: &mut [u8],
    ) -> Result<()> {
        let region_path = self.path_of_region(&Path::Region(*r))?;
        let got = match self.time_for_host() {
            Ok(time_left_100ns) => {
                let access = BufferAccess {
                    space: Space(r.space),
                    address,
                    write,
                    attribute: f.attrib,
                    length: u8::try_from(data.len()).unwrap_or(u8::MAX),
                    connection: None,
                    bit_offset: f.bit_offset % 8,
                    bit_length: f.bit_len,
                    region: &region_path,
                    time_left_100ns,
                };
                let got = self.host.transfer(&access, data);
                self.trace(&Trace::Transfer(&access, got));
                got.map(|_| ()).map_err(|cause| {
                    Error::from(ErrorKind::Region {
                        space: r.space,
                        address,
                        cause,
                    })
                })
            }
            Err(e) => Err(e),
        };
        mem::release(region_path, &mut self.budget);
        got
    }

    fn pins(&mut self, f: &FieldUnit, r: &Region, write: Option<&[u8]>) -> Result<Val> {
        if f.bit_offset / 8 >= r.length {
            return err(ErrorKind::Range);
        }
        let bytes = usize::try_from(f.bit_len.div_ceil(8)).map_err(|_| ErrorKind::Range)?;
        let mut data = mem::zeroed(bytes, &mut self.budget)?;
        if let Some(src) = write {
            for (d, s) in data.iter_mut().zip(src) {
                *d = *s;
            }
        }
        let conn = self.connection_bytes(f)?;
        let region_path = self.path_of_region(&Path::Region(*r))?;
        let address = r
            .offset
            .checked_add(f.bit_offset / 8)
            .ok_or(ErrorKind::Range)?;
        let got = match self.time_for_host() {
            Ok(time_left_100ns) => {
                let access = BufferAccess {
                    space: Space(r.space),
                    address,
                    write: write.is_some(),
                    attribute: f.attrib,
                    length: f.access_len,
                    connection: conn.as_deref(),
                    bit_offset: f.conn_bit_offset,
                    bit_length: f.bit_len,
                    region: &region_path,
                    time_left_100ns,
                };
                let got = self.host.transfer(&access, &mut data);
                self.trace(&Trace::Transfer(&access, got));
                got.map_err(|cause| {
                    Error::from(ErrorKind::Region {
                        space: r.space,
                        address,
                        cause,
                    })
                })
            }
            Err(e) => Err(e),
        };
        mem::release(region_path, &mut self.budget);
        if let Some(c) = conn {
            mem::release(c, &mut self.budget);
        }
        if let Err(e) = got {
            mem::release(data, &mut self.budget);
            return Err(e);
        }
        if write.is_some() {
            mem::release(data, &mut self.budget);
            return Ok(Val::None);
        }
        self.field_value(f.bit_len, data)
    }
}
