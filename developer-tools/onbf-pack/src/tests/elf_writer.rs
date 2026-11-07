//! A test-only ELF64 writer that crafts minimal images in memory.
//!
//! [`RawElf`] writes exactly the header fields and program headers it is given over a file
//! image, for malformed inputs and hand-placed layouts. [`TestElf`] builds a well-formed PIE
//! from a list of loads plus relocations: page 0 holds the ELF header and program headers;
//! each load's bytes start on the next page; the RELA, RELR and dynamic tables go in an
//! extra R load placed after the last one, as `rust-lld` places `.rela.dyn` in an R segment.

pub(crate) const PT_LOAD: u32 = 1;
pub(crate) const PT_DYNAMIC: u32 = 2;
pub(crate) const PT_GNU_STACK: u32 = 0x6474_E551;
pub(crate) const R: u32 = 4;
pub(crate) const RX: u32 = 5;
pub(crate) const RW: u32 = 6;
pub(crate) const RELATIVE: u64 = 8;

/// One `Elf64_Phdr`.
#[derive(Clone, Debug)]
pub(crate) struct Phdr {
    pub(crate) p_type: u32,
    pub(crate) flags: u32,
    pub(crate) offset: u64,
    pub(crate) vaddr: u64,
    pub(crate) filesz: u64,
    pub(crate) memsz: u64,
    pub(crate) align: u64,
}

impl Phdr {
    /// A non-load header that covers nothing.
    pub(crate) fn other(p_type: u32, flags: u32) -> Self {
        Self {
            p_type,
            flags,
            offset: 0,
            vaddr: 0,
            filesz: 0,
            memsz: 0,
            align: 8,
        }
    }
}

/// Header fields plus a file image; [`RawElf::bytes`] writes the header and the program
/// header table (at offset 64) over the start of the image.
#[derive(Clone, Debug)]
pub(crate) struct RawElf {
    pub(crate) class: u8,
    pub(crate) data: u8,
    pub(crate) ident_version: u8,
    pub(crate) e_type: u16,
    pub(crate) machine: u16,
    pub(crate) version: u32,
    pub(crate) entry: u64,
    pub(crate) ehsize: u16,
    pub(crate) phentsize: u16,
    pub(crate) phdrs: Vec<Phdr>,
    pub(crate) file: Vec<u8>,
}

impl RawElf {
    /// An ELF64 LSB x86-64 `ET_DYN` with no program headers and an empty file image.
    pub(crate) fn new() -> Self {
        Self {
            class: 2,
            data: 1,
            ident_version: 1,
            e_type: 3,
            machine: 62,
            version: 1,
            entry: 0,
            ehsize: 64,
            phentsize: 56,
            phdrs: Vec::new(),
            file: Vec::new(),
        }
    }

    /// Writes `bytes` at `offset` in the file image, growing it as needed.
    pub(crate) fn put(&mut self, offset: u64, bytes: &[u8]) {
        let at = offset as usize;
        if self.file.len() < at + bytes.len() {
            self.file.resize(at + bytes.len(), 0);
        }
        self.file[at..at + bytes.len()].copy_from_slice(bytes);
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut out = self.file.clone();
        let table_end = 64 + 56 * self.phdrs.len();
        if out.len() < table_end {
            out.resize(table_end, 0);
        }
        out[0..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);
        out[4] = self.class;
        out[5] = self.data;
        out[6] = self.ident_version;
        out[7..16].fill(0);
        put16(&mut out, 16, self.e_type);
        put16(&mut out, 18, self.machine);
        put32(&mut out, 20, self.version);
        put64(&mut out, 24, self.entry);
        put64(&mut out, 32, 64);
        put64(&mut out, 40, 0);
        put32(&mut out, 48, 0);
        put16(&mut out, 52, self.ehsize);
        put16(&mut out, 54, self.phentsize);
        put16(&mut out, 56, self.phdrs.len() as u16);
        put16(&mut out, 58, 64);
        put16(&mut out, 60, 0);
        put16(&mut out, 62, 0);
        for (i, p) in self.phdrs.iter().enumerate() {
            let at = 64 + 56 * i;
            put32(&mut out, at, p.p_type);
            put32(&mut out, at + 4, p.flags);
            put64(&mut out, at + 8, p.offset);
            put64(&mut out, at + 16, p.vaddr);
            put64(&mut out, at + 24, p.vaddr);
            put64(&mut out, at + 32, p.filesz);
            put64(&mut out, at + 40, p.memsz);
            put64(&mut out, at + 48, p.align);
        }
        out
    }
}

/// One loadable segment for [`TestElf`].
#[derive(Clone, Debug)]
pub(crate) struct Load {
    pub(crate) flags: u32,
    pub(crate) vaddr: u64,
    pub(crate) data: Vec<u8>,
    pub(crate) mem_size: u64,
}

impl Load {
    pub(crate) fn new(flags: u32, vaddr: u64, data: Vec<u8>) -> Self {
        let mem_size = data.len() as u64;
        Self {
            flags,
            vaddr,
            data,
            mem_size,
        }
    }

    pub(crate) fn mem(mut self, mem_size: u64) -> Self {
        self.mem_size = mem_size;
        self
    }
}

/// A well-formed PIE description; see the module docs for the layout.
#[derive(Clone, Debug)]
pub(crate) struct TestElf {
    pub(crate) entry: u64,
    pub(crate) loads: Vec<Load>,
    /// Extra program headers, after the loads and `PT_DYNAMIC`.
    pub(crate) extra: Vec<Phdr>,
    /// `(r_offset, r_info, r_addend)`.
    pub(crate) rela: Vec<(u64, u64, u64)>,
    /// Raw RELR words.
    pub(crate) relr: Vec<u64>,
    /// Extra dynamic entries, before `DT_NULL`.
    pub(crate) dynamic: Vec<(u64, u64)>,
    pub(crate) relaent: u64,
    pub(crate) relrent: u64,
    /// Emit `PT_DYNAMIC` even with no relocations.
    pub(crate) force_dynamic: bool,
}

/// Text that starts at its entry point: `pause; jmp $-2` padded with `int3`.
pub(crate) fn text(len: usize) -> Vec<u8> {
    let mut t = vec![0xCC; len];
    t[..4].copy_from_slice(&[0xF3, 0x90, 0xEB, 0xFC]);
    t
}

impl TestElf {
    /// RX text at 0x1000 (entry), R data at 0x2000, RW data at 0x3000 with bss.
    pub(crate) fn kernel() -> Self {
        Self {
            entry: 0x1000,
            loads: vec![
                Load::new(RX, 0x1000, text(64)),
                Load::new(R, 0x2000, b"origin read-only data".to_vec()),
                Load::new(RW, 0x3000, vec![0; 64]).mem(0x100),
            ],
            extra: vec![Phdr::other(PT_GNU_STACK, RW)],
            rela: Vec::new(),
            relr: Vec::new(),
            dynamic: Vec::new(),
            relaent: 24,
            relrent: 8,
            force_dynamic: false,
        }
    }

    /// The first RVA after every load, page-aligned: where the table segment goes.
    pub(crate) fn meta_vaddr(&self) -> u64 {
        let end = self
            .loads
            .iter()
            .map(|l| l.vaddr + l.mem_size)
            .max()
            .unwrap_or(0);
        end.div_ceil(4096) * 4096
    }

    pub(crate) fn raw(&self) -> RawElf {
        let mut raw = RawElf::new();
        raw.entry = self.entry;
        let mut offset = 4096u64;
        for l in &self.loads {
            raw.put(offset, &l.data);
            raw.phdrs.push(Phdr {
                p_type: PT_LOAD,
                flags: l.flags,
                offset,
                vaddr: l.vaddr,
                filesz: l.data.len() as u64,
                memsz: l.mem_size,
                align: 4096,
            });
            offset += (l.data.len() as u64).div_ceil(4096).max(1) * 4096;
        }

        let has_dynamic = self.force_dynamic
            || !self.rela.is_empty()
            || !self.relr.is_empty()
            || !self.dynamic.is_empty();
        if has_dynamic {
            let base = self.meta_vaddr();
            let mut meta = Vec::new();
            let mut tags = Vec::new();
            if !self.rela.is_empty() {
                tags.push((7, base));
                tags.push((8, 24 * self.rela.len() as u64));
                tags.push((9, self.relaent));
                tags.push((0x6FFF_FFF9, self.rela.len() as u64));
                for &(r_offset, info, addend) in &self.rela {
                    meta.extend_from_slice(&r_offset.to_le_bytes());
                    meta.extend_from_slice(&info.to_le_bytes());
                    meta.extend_from_slice(&addend.to_le_bytes());
                }
            }
            if !self.relr.is_empty() {
                tags.push((36, base + meta.len() as u64));
                tags.push((35, 8 * self.relr.len() as u64));
                tags.push((37, self.relrent));
                for w in &self.relr {
                    meta.extend_from_slice(&w.to_le_bytes());
                }
            }
            tags.extend_from_slice(&self.dynamic);
            tags.push((0, 0));
            let dynamic_rva = base + meta.len() as u64;
            for (tag, value) in &tags {
                meta.extend_from_slice(&tag.to_le_bytes());
                meta.extend_from_slice(&value.to_le_bytes());
            }
            raw.put(offset, &meta);
            raw.phdrs.push(Phdr {
                p_type: PT_LOAD,
                flags: R,
                offset,
                vaddr: base,
                filesz: meta.len() as u64,
                memsz: meta.len() as u64,
                align: 4096,
            });
            let dynamic_size = 16 * tags.len() as u64;
            raw.phdrs.push(Phdr {
                p_type: PT_DYNAMIC,
                flags: R,
                offset: offset + dynamic_rva - base,
                vaddr: dynamic_rva,
                filesz: dynamic_size,
                memsz: dynamic_size,
                align: 8,
            });
        }
        raw.phdrs.extend(self.extra.iter().cloned());
        raw
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.raw().bytes()
    }
}

pub(crate) fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
pub(crate) fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
pub(crate) fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
