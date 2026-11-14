//! Field units: reading and writing operation regions through the host (ACPI 6.6 §5.5.2.4,
//! §19.6.7, §19.6.48, §19.6.64, §19.3.5.7, Tables 5.221 and 19.34).
//!
//! - **Byte-addressed spaces** (SystemMemory, SystemIO, PCI_Config, EmbeddedControl, SystemCMOS,
//!   PciBarTarget, OEM): a field is read and written in naturally aligned units of its access
//!   width, each unit one host access inside the region. A unit the field covers only in part is
//!   written by the update rule: Preserve reads it first, WriteAsOnes and WriteAsZeros fill the
//!   rest. EmbeddedControl and SystemCMOS take bytes only. `AnyAcc` uses the narrowest naturally
//!   aligned width that holds the whole field, else bytes (G5, strict until the reading session
//!   answers).
//! - **Index fields** write the unit's byte offset to the index field, then read or write the data
//!   field; **bank fields** write the bank value to the bank field first. A chain of fields is at
//!   most [`FIELD_DEPTH`] deep.
//! - **Buffer-protocol spaces** (SMBus, IPMI, GenericSerialBus, PlatformRtMechanism, FFixedHW):
//!   one host transfer of the space's data buffer, with the field's access attribute and its
//!   `Connection`; the result is the buffer the host filled.
//! - **GeneralPurposeIO and PCC**: one host transfer of the field's bytes.
//! - **PCI_Config regions** name their function: the segment and bus of the PCI root bridge above
//!   them (`_SEG`, `_BBN`), a bridge's secondary bus read for each bridge between, and the
//!   function's `_ADR`, found at the first access and kept.
//! - A field with the `Lock` rule takes the Global Lock around its access.
//!
//! Whether the region may be touched at all is the host's decision (ADR-0019 A2; `host.rs`).

use alloc::vec::Vec;

use crate::Interpreter;
use crate::error::{Error, ErrorKind, Result, err};
use crate::host::{BufferAccess, Host, HostError, PciAddress, RegionAccess, Space};
use crate::mem;
use crate::name::Seg;
use crate::object::{Connection, FieldKind, FieldUnit, NodeId, Object, Region, Val};
use crate::store::{get_bits, set_bits};

/// The deepest chain of field units one access goes through.
pub(crate) const FIELD_DEPTH: u32 = 8;

/// `PNP0A03` and `PNP0A08` as EISA IDs: a PCI and a PCI Express root bridge.
const ROOT_BRIDGES: [u64; 2] = [0x030A_D041, 0x080A_D041];

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
    match f.access {
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
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
        if r.space == 2 && r.pci.is_none() {
            let pci = self.pci_address(node)?;
            if let Object::Region(stored) = self.heap.get_mut(obj)? {
                stored.pci = Some(pci);
            }
            return Ok(Region {
                pci: Some(pci),
                ..r
            });
        }
        Ok(r)
    }

    /// A field unit's value (§19.3.5.7: an integer if it fits one, else a buffer).
    pub(crate) fn read_field(&mut self, node: NodeId) -> Result<Val> {
        self.field_at(node, None, 0)
    }

    /// Writes `v` to a field unit. For a buffer-protocol field, the buffer the host returned,
    /// which is `Store`'s result (§19.6.132).
    pub(crate) fn write_field(&mut self, node: NodeId, v: Val) -> Result<Option<Val>> {
        self.write_value(node, v, 0)
    }

    fn write_value(&mut self, node: NodeId, v: Val, depth: u32) -> Result<Option<Val>> {
        let f = self.unit_of(node)?;
        let bytes = usize::try_from(f.bit_len.div_ceil(8)).map_err(|_| ErrorKind::Range)?;
        // A string is written a character at a time (Table 19.7).
        if let Val::Obj(r) = v
            && let Object::String(s) = self.heap.get(r)?
        {
            let chars = mem::copy_bytes(s, &mut self.budget)?;
            let mut out = Ok(None);
            for &c in &chars {
                out = self.field_at(node, Some(&[c]), depth).map(|_| None);
                if out.is_err() {
                    break;
                }
            }
            mem::release(chars, &mut self.budget);
            return out;
        }
        let data = match v {
            Val::Int(i) => mem::copy_bytes(&i.to_le_bytes(), &mut self.budget)?,
            other => self.buffer_of(other)?,
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
        } else {
            // A buffer larger than the field is written in pieces, lower first (Table 19.7).
            let mut r = Ok(None);
            let bits = u64::try_from(data.len()).unwrap_or(0).saturating_mul(8);
            let mut at: u64 = 0;
            let mut piece = mem::zeroed(bytes, &mut self.budget)?;
            while at < bits && r.is_ok() {
                piece.fill(0);
                let n = f.bit_len.min(bits.saturating_sub(at));
                r = get_bits(&data, at, n, &mut piece)
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
                let access = self.access(&r, region_path, byte_off, w)?;
                self.host.read(&access).map_err(|cause| {
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
                let access = self.access(&r, region_path, byte_off, w)?;
                self.host.write(&access, value).map_err(|cause| {
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
        })
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
        let access = BufferAccess {
            space: Space(r.space),
            address,
            write: write.is_some(),
            attribute: f.attrib,
            length: f.access_len,
            connection: conn.as_deref(),
            region: &region_path,
        };
        let got = self.host.transfer(&access, &mut data);
        mem::release(region_path, &mut self.budget);
        if let Some(c) = conn {
            mem::release(c, &mut self.budget);
        }
        let filled = match got {
            Ok(n) => n.min(size),
            Err(cause) => {
                mem::release(data, &mut self.budget);
                return err(ErrorKind::Region {
                    space: r.space,
                    address,
                    cause,
                });
            }
        };
        data.truncate(filled);
        self.new_buffer(data)
    }

    /// One transfer of a field's own bytes (GeneralPurposeIO, PCC).
    fn pins(&mut self, f: &FieldUnit, r: &Region, write: Option<&[u8]>) -> Result<Val> {
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
        let access = BufferAccess {
            space: Space(r.space),
            address,
            write: write.is_some(),
            attribute: f.attrib,
            length: f.access_len,
            connection: conn.as_deref(),
            region: &region_path,
        };
        let got = self.host.transfer(&access, &mut data);
        mem::release(region_path, &mut self.budget);
        if let Some(c) = conn {
            mem::release(c, &mut self.budget);
        }
        if let Err(cause) = got {
            mem::release(data, &mut self.budget);
            return err(ErrorKind::Region {
                space: r.space,
                address,
                cause,
            });
        }
        if write.is_some() {
            mem::release(data, &mut self.budget);
            return Ok(Val::None);
        }
        self.field_value(f.bit_len, data)
    }

    // --- PCI_Config: which function --------------------------------------------------------

    /// `seg`'s value under `node`, run if a method; `None` if absent.
    fn child_value(&mut self, node: NodeId, seg: Seg) -> Result<Option<Val>> {
        match self.ns.child(node, seg) {
            Some(c) => Ok(Some(self.value_of(c)?)),
            None => Ok(None),
        }
    }

    fn child_int(&mut self, node: NodeId, seg: Seg) -> Result<Option<u64>> {
        match self.child_value(node, seg)? {
            Some(v) => Ok(Some(self.int_of(v)?)),
            None => Ok(None),
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
                Ok(Object::Package(e)) => e.iter().any(|&x| one(x)),
                _ => one(v),
            },
            _ => one(v),
        }
    }

    fn is_root_bridge(&mut self, node: NodeId) -> Result<bool> {
        for seg in [*b"_HID", *b"_CID"] {
            if let Some(v) = self.child_value(node, seg)?
                && self.names_root_bridge(v)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The function a PCI_Config region declared in `region`'s scope names.
    fn pci_address(&mut self, region: NodeId) -> Result<PciAddress> {
        let device = self.ns.parent(region).ok_or(ErrorKind::NotFound)?;
        let mut chain: Vec<NodeId> = Vec::new();
        let result = self.pci_walk(device, &mut chain);
        mem::release(chain, &mut self.budget);
        result
    }

    fn pci_walk(&mut self, device: NodeId, chain: &mut Vec<NodeId>) -> Result<PciAddress> {
        let mut n = device;
        let mut root = None;
        loop {
            if self.is_root_bridge(n)? {
                root = Some(n);
                break;
            }
            mem::push(chain, n, &mut self.budget)?;
            match self.ns.parent(n) {
                Some(p) if p.index != 0 => n = p,
                _ => break,
            }
        }
        let (segment, mut bus) = match root {
            Some(r) => (
                self.child_int(r, *b"_SEG")?.unwrap_or(0),
                self.child_int(r, *b"_BBN")?.unwrap_or(0),
            ),
            None => (0, 0),
        };
        let target = if chain.is_empty() {
            root.unwrap_or(device)
        } else {
            device
        };
        let adr = self.child_int(target, *b"_ADR")?.unwrap_or(0);
        let segment = u16::try_from(segment & 0xFFFF).unwrap_or(0);
        // Down from the root: each bridge between it and the function gives the next bus. With no
        // root bridge above, there are no bridges to read.
        let bridges = if root.is_some() {
            chain.len().saturating_sub(1)
        } else {
            0
        };
        for k in (1..=bridges).rev() {
            let Some(&bridge) = chain.get(k) else { break };
            let badr = self.child_int(bridge, *b"_ADR")?.unwrap_or(0);
            let here = PciAddress {
                segment,
                bus: u8::try_from(bus & 0xFF).unwrap_or(0),
                device: u8::try_from((badr >> 16) & 0xFF).unwrap_or(0),
                function: u8::try_from(badr & 0xFF).unwrap_or(0),
            };
            let access = RegionAccess {
                space: Space::PCI_CONFIG,
                address: 0x19,
                width: 8,
                pci: Some(here),
                region: &[],
            };
            bus = self
                .host
                .read(&access)
                .map_err(|cause: HostError| ErrorKind::Region {
                    space: 2,
                    address: 0x19,
                    cause,
                })?
                & 0xFF;
        }
        Ok(PciAddress {
            segment,
            bus: u8::try_from(bus & 0xFF).unwrap_or(0),
            device: u8::try_from((adr >> 16) & 0xFF).unwrap_or(0),
            function: u8::try_from(adr & 0xFF).unwrap_or(0),
        })
    }
}
