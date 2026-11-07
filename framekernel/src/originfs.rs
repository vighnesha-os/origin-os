//! OriginFS v0: a read-only on-disk filesystem parser and an image builder (ADR-0006,
//! **Proposed**). The on-disk format decision needs the owner's approval before it is
//! Accepted; this parser is host-testable and **not mounted by the shipping kernel**
//! (CLAUDE.md §84). It will plug in beneath `framekernel::vfs` once the ADR is Accepted.
//!
//! Layout (ADR-0006): a 4 KiB block, little-endian. Block 0 is the superblock; a fixed
//! inode table follows; data blocks follow that. An inode has 12 direct block pointers and
//! one single-indirect, reaching ~2 MiB. Directories are linked `(inode, rec_len, name)`
//! records. Every field is range-checked and no parse panics on a malformed image.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gpt::crc32;
use crate::vfs::{self, Component};

/// The filesystem block size (ADR-0006 F1).
pub const BLOCK: usize = 4096;
/// Bytes per inode (F3), and inodes per block.
pub const INODE_SIZE: usize = 128;
pub const INODES_PER_BLOCK: u64 = (BLOCK / INODE_SIZE) as u64;
/// u64 block pointers per single-indirect block.
pub const POINTERS_PER_BLOCK: u64 = (BLOCK / 8) as u64;
/// Direct pointers in an inode.
pub const DIRECT: usize = 12;
/// The superblock magic.
pub const MAGIC: [u8; 8] = *b"ORIGINFS";

// Inode field byte offsets (ADR-0006 F3): kind u16, _reserved u16, links u32, size u64,
// blocks u64, then 12 direct u64 pointers and one single-indirect u64.
const OFF_KIND: usize = 0;
const OFF_SIZE: usize = 8;
const OFF_DIRECT: usize = 24;
const OFF_INDIRECT: usize = OFF_DIRECT + DIRECT * 8; // 120

/// Inode kinds (F3).
pub mod kind {
    pub const FILE: u16 = 1;
    pub const DIRECTORY: u16 = 2;
}

/// Why an image or lookup was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    BadSuperblock,
    BadInode(u32),
    NotFound,
    NotDirectory,
    IsDirectory,
    BadPath,
    /// A block read failed (the device returned no data).
    Io,
    /// A structure referenced a block outside the volume.
    OutOfRange,
}

/// A source of 4 KiB blocks.
pub trait BlockDevice {
    /// Reads block `block` into `out` (at least [`BLOCK`] bytes); returns whether it
    /// succeeded.
    fn read_block(&self, block: u64, out: &mut [u8]) -> bool;
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut v = [0u8; 4];
    v.copy_from_slice(b.get(at..at + 4).unwrap_or(&[0; 4]));
    u32::from_le_bytes(v)
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(b.get(at..at + 8).unwrap_or(&[0; 8]));
    u64::from_le_bytes(v)
}

/// The validated superblock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Superblock {
    pub total_blocks: u64,
    pub inode_count: u64,
    pub inode_table_start: u64,
    pub data_start: u64,
    pub root_inode: u32,
}

impl Superblock {
    /// The length of the superblock prefix the CRC covers (the CRC field follows it).
    const CRC_SPAN: usize = 60;
    const CRC_AT: usize = 60;

    fn parse(block: &[u8]) -> Result<Superblock, FsError> {
        if block.get(..8) != Some(&MAGIC[..]) || u32_at(block, 8) != 1 {
            return Err(FsError::BadSuperblock);
        }
        if u32_at(block, 12) as usize != BLOCK {
            return Err(FsError::BadSuperblock);
        }
        let stored = u32_at(block, Self::CRC_AT);
        if crc32(block.get(..Self::CRC_SPAN).unwrap_or(&[])) != stored {
            return Err(FsError::BadSuperblock);
        }
        let sb = Superblock {
            total_blocks: u64_at(block, 16),
            inode_count: u64_at(block, 24),
            inode_table_start: u64_at(block, 32),
            data_start: u64_at(block, 40),
            root_inode: u32_at(block, 48),
        };
        // The inode table must lie before the data region, which lies inside the volume.
        let table_blocks = sb.inode_count.div_ceil(INODES_PER_BLOCK);
        if sb.inode_table_start < 1
            || sb.inode_table_start + table_blocks > sb.data_start
            || sb.data_start > sb.total_blocks
            || sb.root_inode == 0
            || u64::from(sb.root_inode) >= sb.inode_count
        {
            return Err(FsError::BadSuperblock);
        }
        Ok(sb)
    }

    #[cfg(test)] // builder-only: the shipped read path never encodes a superblock
    fn encode(&self) -> [u8; BLOCK] {
        let mut b = [0u8; BLOCK];
        b[..8].copy_from_slice(&MAGIC);
        b[8..12].copy_from_slice(&1u32.to_le_bytes());
        b[12..16].copy_from_slice(&(BLOCK as u32).to_le_bytes());
        b[16..24].copy_from_slice(&self.total_blocks.to_le_bytes());
        b[24..32].copy_from_slice(&self.inode_count.to_le_bytes());
        b[32..40].copy_from_slice(&self.inode_table_start.to_le_bytes());
        b[40..48].copy_from_slice(&self.data_start.to_le_bytes());
        b[48..52].copy_from_slice(&self.root_inode.to_le_bytes());
        let crc = crc32(&b[..Self::CRC_SPAN]);
        b[Self::CRC_AT..Self::CRC_AT + 4].copy_from_slice(&crc.to_le_bytes());
        b
    }
}

/// A decoded inode (the fields the read path needs; `links`/`blocks` are reserved in v0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inode {
    pub kind: u16,
    pub size: u64,
    pub direct: [u64; DIRECT],
    pub single_indirect: u64,
}

impl Inode {
    fn parse(bytes: &[u8]) -> Inode {
        let mut direct = [0u64; DIRECT];
        for (i, slot) in direct.iter_mut().enumerate() {
            *slot = u64_at(bytes, OFF_DIRECT + i * 8);
        }
        Inode {
            kind: u32_at(bytes, OFF_KIND) as u16,
            size: u64_at(bytes, OFF_SIZE),
            direct,
            single_indirect: u64_at(bytes, OFF_INDIRECT),
        }
    }

    #[cfg(test)] // builder-only: the shipped read path never encodes an inode
    fn encode(&self) -> [u8; INODE_SIZE] {
        let mut b = [0u8; INODE_SIZE];
        b[OFF_KIND..OFF_KIND + 4].copy_from_slice(&u32::from(self.kind).to_le_bytes());
        b[OFF_SIZE..OFF_SIZE + 8].copy_from_slice(&self.size.to_le_bytes());
        for (i, &p) in self.direct.iter().enumerate() {
            b[OFF_DIRECT + i * 8..OFF_DIRECT + i * 8 + 8].copy_from_slice(&p.to_le_bytes());
        }
        b[OFF_INDIRECT..OFF_INDIRECT + 8].copy_from_slice(&self.single_indirect.to_le_bytes());
        b
    }

    pub fn is_dir(&self) -> bool {
        self.kind == kind::DIRECTORY
    }
}

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub inode: u32,
    pub kind: u8,
    pub name: String,
}

/// A mounted, read-only OriginFS over a block device.
pub struct Fs<'a, D: BlockDevice> {
    device: &'a D,
    sb: Superblock,
}

impl<D: BlockDevice> core::fmt::Debug for Fs<'_, D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Fs")
            .field("sb", &self.sb)
            .finish_non_exhaustive()
    }
}

impl<'a, D: BlockDevice> Fs<'a, D> {
    /// Opens the filesystem on `device`, validating the superblock.
    pub fn open(device: &'a D) -> Result<Self, FsError> {
        let mut block = alloc::vec![0u8; BLOCK];
        if !device.read_block(0, &mut block) {
            return Err(FsError::Io);
        }
        let sb = Superblock::parse(&block)?;
        Ok(Self { device, sb })
    }

    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }

    /// Reads inode `n`.
    pub fn inode(&self, n: u32) -> Result<Inode, FsError> {
        if n == 0 || u64::from(n) >= self.sb.inode_count {
            return Err(FsError::BadInode(n));
        }
        let block_no = self.sb.inode_table_start + u64::from(n) / INODES_PER_BLOCK;
        if block_no >= self.sb.data_start {
            return Err(FsError::BadInode(n));
        }
        let mut block = alloc::vec![0u8; BLOCK];
        if !self.device.read_block(block_no, &mut block) {
            return Err(FsError::Io);
        }
        let offset = (u64::from(n) % INODES_PER_BLOCK) as usize * INODE_SIZE;
        Ok(Inode::parse(
            block.get(offset..offset + INODE_SIZE).unwrap_or(&[]),
        ))
    }

    /// The data block holding block `index` of `inode` (0 for a hole), or an error if a
    /// pointer is out of range.
    fn data_block(&self, inode: &Inode, index: u64) -> Result<u64, FsError> {
        let ptr = if index < DIRECT as u64 {
            inode.direct[index as usize]
        } else if index < DIRECT as u64 + POINTERS_PER_BLOCK {
            if inode.single_indirect == 0 {
                0
            } else {
                let mut block = alloc::vec![0u8; BLOCK];
                self.read_data(inode.single_indirect, &mut block)?;
                u64_at(&block, (index - DIRECT as u64) as usize * 8)
            }
        } else {
            return Err(FsError::OutOfRange); // beyond v0's single-indirect reach
        };
        if ptr != 0 && (ptr < self.sb.data_start || ptr >= self.sb.total_blocks) {
            return Err(FsError::OutOfRange);
        }
        Ok(ptr)
    }

    /// Reads block `block` of the volume into `out`, range-checked.
    fn read_data(&self, block: u64, out: &mut [u8]) -> Result<(), FsError> {
        if block >= self.sb.total_blocks {
            return Err(FsError::OutOfRange);
        }
        if !self.device.read_block(block, out) {
            return Err(FsError::Io);
        }
        Ok(())
    }

    /// Reads up to `out.len()` bytes of file inode `n` from `offset`; returns how many were
    /// read (0 at or past the end). A hole reads as zeros.
    pub fn read_file(&self, n: u32, offset: u64, out: &mut [u8]) -> Result<usize, FsError> {
        let inode = self.inode(n)?;
        if inode.is_dir() {
            return Err(FsError::IsDirectory);
        }
        let end = offset.saturating_add(out.len() as u64).min(inode.size);
        if offset >= end {
            return Ok(0);
        }
        let mut done = 0usize;
        let mut pos = offset;
        let mut block = alloc::vec![0u8; BLOCK];
        while pos < end {
            let index = pos / BLOCK as u64;
            let within = (pos % BLOCK as u64) as usize;
            let ptr = self.data_block(&inode, index)?;
            if ptr == 0 {
                block.iter_mut().for_each(|b| *b = 0); // a hole
            } else {
                self.read_data(ptr, &mut block)?;
            }
            let n = ((end - pos) as usize).min(BLOCK - within);
            out.get_mut(done..done + n)
                .ok_or(FsError::OutOfRange)?
                .copy_from_slice(&block[within..within + n]);
            done += n;
            pos += n as u64;
        }
        Ok(done)
    }

    /// The whole contents of file inode `n`.
    pub fn read_whole(&self, n: u32) -> Result<Vec<u8>, FsError> {
        let inode = self.inode(n)?;
        if inode.is_dir() {
            return Err(FsError::IsDirectory);
        }
        // A v0 file addresses at most 12 direct + 512 single-indirect blocks; a size beyond
        // that is a corrupt inode, refused before it drives an unbounded allocation.
        let max = (DIRECT as u64 + POINTERS_PER_BLOCK) * BLOCK as u64;
        if inode.size > max {
            return Err(FsError::OutOfRange);
        }
        let mut out = alloc::vec![0u8; inode.size as usize];
        self.read_file(n, 0, &mut out)?;
        Ok(out)
    }

    /// The entries of directory inode `n`.
    pub fn entries(&self, n: u32) -> Result<Vec<DirEntry>, FsError> {
        let inode = self.inode(n)?;
        if !inode.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let mut out = Vec::new();
        let mut block = alloc::vec![0u8; BLOCK];
        let blocks = inode.size.div_ceil(BLOCK as u64);
        for index in 0..blocks {
            let ptr = self.data_block(&inode, index)?;
            if ptr == 0 {
                continue;
            }
            self.read_data(ptr, &mut block)?;
            let mut at = 0usize;
            while at + 8 <= BLOCK {
                let entry_inode = u32_at(&block, at);
                let rec_len = u16::from_le_bytes([block[at + 4], block[at + 5]]) as usize;
                let name_len = block[at + 6] as usize;
                let ekind = block[at + 7];
                if rec_len < 8 || at + rec_len > BLOCK {
                    break; // malformed, or padding to the block end
                }
                if entry_inode != 0 && at + 8 + name_len <= BLOCK {
                    let name = core::str::from_utf8(&block[at + 8..at + 8 + name_len])
                        .unwrap_or("")
                        .into();
                    out.push(DirEntry {
                        inode: entry_inode,
                        kind: ekind,
                        name,
                    });
                }
                at += rec_len;
            }
        }
        Ok(out)
    }

    /// Resolves `path` to an inode number, following `.` and `..` (the root's parent is
    /// itself).
    pub fn resolve(&self, path: &str) -> Result<u32, FsError> {
        let comps = vfs::components(path).ok_or(FsError::BadPath)?;
        let mut here = self.sb.root_inode;
        let mut parent = self.sb.root_inode;
        for comp in comps {
            match comp {
                Component::Current => {}
                Component::Parent => here = parent,
                Component::Name(name) => {
                    let entry = self
                        .entries(here)?
                        .into_iter()
                        .find(|e| e.name == name)
                        .ok_or(FsError::NotFound)?;
                    parent = here;
                    here = entry.inode;
                }
            }
        }
        Ok(here)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node to place in a built image: `parent` is its directory's inode (the root, inode
    /// 1, is its own parent); the inode number is the node's index + 1.
    struct Node {
        parent: u32,
        name: String,
        is_dir: bool,
        content: Vec<u8>,
    }

    /// Builds an in-memory OriginFS image: a root directory, named files with content, and
    /// subdirectories. A deliberately small, correct layout, exercising direct and
    /// single-indirect file blocks. Directories are kept to one block.
    struct Builder {
        nodes: Vec<Node>,
    }

    /// An in-memory block device over a byte image.
    #[derive(Clone)]
    struct Image(Vec<u8>);
    impl BlockDevice for Image {
        fn read_block(&self, block: u64, out: &mut [u8]) -> bool {
            let at = block as usize * BLOCK;
            match self.0.get(at..at + BLOCK) {
                Some(src) => {
                    out[..BLOCK].copy_from_slice(src);
                    true
                }
                None => false,
            }
        }
    }

    impl Builder {
        /// A fresh image with just the root directory (inode 1, its own parent).
        fn new() -> Self {
            Self {
                nodes: alloc::vec![Node {
                    parent: 1,
                    name: String::from("/"),
                    is_dir: true,
                    content: Vec::new(),
                }],
            }
        }

        /// Adds a child of directory inode `parent`; returns the new node's inode number.
        fn add(&mut self, parent: u32, name: &str, is_dir: bool, content: &[u8]) -> u32 {
            self.nodes.push(Node {
                parent,
                name: name.into(),
                is_dir,
                content: content.to_vec(),
            });
            self.nodes.len() as u32
        }

        /// The one-block directory listing for directory inode `dir`: `.`, `..`, then each
        /// child, with `rec_len`s packed 4-aligned and the last record filling the block.
        fn dir_block(&self, dir: u32) -> Vec<u8> {
            let mut buf = alloc::vec![0u8; BLOCK];
            let parent = self.nodes[dir as usize - 1].parent;
            let mut entries: Vec<(u32, u8, &str)> = alloc::vec![
                (dir, kind::DIRECTORY as u8, "."),
                (parent, kind::DIRECTORY as u8, ".."),
            ];
            for (i, node) in self.nodes.iter().enumerate() {
                let inum = i as u32 + 1;
                if node.parent == dir && inum != dir {
                    let k = if node.is_dir {
                        kind::DIRECTORY
                    } else {
                        kind::FILE
                    } as u8;
                    entries.push((inum, k, node.name.as_str()));
                }
            }
            let mut at = 0usize;
            let last = entries.len() - 1;
            for (i, &(inode, k, name)) in entries.iter().enumerate() {
                let rec = if i == last {
                    BLOCK - at
                } else {
                    8 + name.len().div_ceil(4) * 4
                };
                buf[at..at + 4].copy_from_slice(&inode.to_le_bytes());
                buf[at + 4..at + 6].copy_from_slice(&(rec as u16).to_le_bytes());
                buf[at + 6] = name.len() as u8;
                buf[at + 7] = k;
                buf[at + 8..at + 8 + name.len()].copy_from_slice(name.as_bytes());
                at += rec;
            }
            buf
        }

        /// Serializes the whole image.
        fn build(&self) -> Vec<u8> {
            let inode_count = self.nodes.len() as u64 + 1; // inode 0 is unused
            let inode_table_start = 1u64;
            let data_start = inode_table_start + inode_count.div_ceil(INODES_PER_BLOCK);

            let mut next = data_start;
            let mut writes: Vec<(u64, Vec<u8>)> = Vec::new();
            let mut inodes: Vec<(u32, Inode)> = Vec::new();

            for (i, node) in self.nodes.iter().enumerate() {
                let inum = i as u32 + 1;
                let data = if node.is_dir {
                    self.dir_block(inum)
                } else {
                    node.content.clone()
                };
                let size = if node.is_dir {
                    BLOCK as u64
                } else {
                    node.content.len() as u64
                };
                let nblocks = if node.is_dir {
                    1
                } else {
                    node.content.len().div_ceil(BLOCK)
                };

                let mut block_nums = Vec::new();
                for bi in 0..nblocks {
                    let b = next;
                    next += 1;
                    block_nums.push(b);
                    let start = bi * BLOCK;
                    let end = (start + BLOCK).min(data.len());
                    let mut buf = alloc::vec![0u8; BLOCK];
                    buf[..end - start].copy_from_slice(&data[start..end]);
                    writes.push((b, buf));
                }

                let mut direct = [0u64; DIRECT];
                for (k, &b) in block_nums.iter().take(DIRECT).enumerate() {
                    direct[k] = b;
                }
                let mut single_indirect = 0u64;
                if nblocks > DIRECT {
                    let ib = next;
                    next += 1;
                    single_indirect = ib;
                    let mut buf = alloc::vec![0u8; BLOCK];
                    for (k, &b) in block_nums[DIRECT..].iter().enumerate() {
                        buf[k * 8..k * 8 + 8].copy_from_slice(&b.to_le_bytes());
                    }
                    writes.push((ib, buf));
                }

                inodes.push((
                    inum,
                    Inode {
                        kind: if node.is_dir {
                            kind::DIRECTORY
                        } else {
                            kind::FILE
                        },
                        size,
                        direct,
                        single_indirect,
                    },
                ));
            }

            let total_blocks = next;
            let mut img = alloc::vec![0u8; total_blocks as usize * BLOCK];
            let sb = Superblock {
                total_blocks,
                inode_count,
                inode_table_start,
                data_start,
                root_inode: 1,
            };
            img[..BLOCK].copy_from_slice(&sb.encode());
            for (inum, inode) in &inodes {
                let off = inode_table_start as usize * BLOCK + *inum as usize * INODE_SIZE;
                img[off..off + INODE_SIZE].copy_from_slice(&inode.encode());
            }
            for (b, buf) in &writes {
                let off = *b as usize * BLOCK;
                img[off..off + BLOCK].copy_from_slice(buf);
            }
            img
        }
    }

    fn names(fs: &Fs<'_, Image>, dir: u32) -> Vec<String> {
        let mut n: Vec<String> = fs
            .entries(dir)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        n.sort();
        n
    }

    #[test]
    fn builds_parses_and_reads_a_tree() {
        let mut b = Builder::new();
        b.add(1, "hello.txt", false, b"Hello, OriginFS!\n");
        let etc = b.add(1, "etc", true, &[]);
        b.add(etc, "hostname", false, b"origin\n");
        // A file that spills past the 12 direct pointers into the single-indirect block.
        let big_len = 15 * BLOCK + 123;
        let big: Vec<u8> = (0..big_len).map(|i| (i % 251) as u8).collect();
        b.add(1, "big.bin", false, &big);
        let fs_dev = Image(b.build());
        let fs = Fs::open(&fs_dev).unwrap();

        assert_eq!(fs.superblock().root_inode, 1);
        assert_eq!(names(&fs, 1), [".", "..", "big.bin", "etc", "hello.txt"]);

        // Small file, nested file, and `.`/`..` resolution.
        let hello = fs.resolve("/hello.txt").unwrap();
        assert_eq!(fs.read_whole(hello).unwrap(), b"Hello, OriginFS!\n");
        let hostname = fs.resolve("/etc/hostname").unwrap();
        assert_eq!(fs.read_whole(hostname).unwrap(), b"origin\n");
        assert_eq!(fs.resolve("/etc/../etc/./hostname").unwrap(), hostname);
        assert_eq!(names(&fs, etc), [".", "..", "hostname"]);

        // The large file, read whole (direct + single-indirect) and byte-exact.
        let big_ino = fs.resolve("/big.bin").unwrap();
        let got = fs.read_whole(big_ino).unwrap();
        assert_eq!(got.len(), big_len);
        assert_eq!(got, big);

        // A partial read straddling a block boundary.
        let mut part = [0u8; 10];
        let n = fs.read_file(big_ino, BLOCK as u64 - 3, &mut part).unwrap();
        assert_eq!(n, 10);
        assert_eq!(part, big[BLOCK - 3..BLOCK + 7]);
        // At or past the end reads nothing.
        assert_eq!(fs.read_file(big_ino, big_len as u64, &mut part).unwrap(), 0);
    }

    #[test]
    fn resolution_and_kind_errors() {
        let mut b = Builder::new();
        b.add(1, "dir", true, &[]);
        b.add(1, "file", false, b"x");
        let fs_dev = Image(b.build());
        let fs = Fs::open(&fs_dev).unwrap();

        assert_eq!(fs.resolve("/nope"), Err(FsError::NotFound));
        assert_eq!(fs.resolve("relative"), Err(FsError::BadPath));
        // Reading a directory, and listing a file, are refused distinctly.
        let dir = fs.resolve("/dir").unwrap();
        let file = fs.resolve("/file").unwrap();
        assert_eq!(fs.read_whole(dir), Err(FsError::IsDirectory));
        assert_eq!(fs.entries(file), Err(FsError::NotDirectory));
        assert_eq!(fs.inode(0), Err(FsError::BadInode(0)));
    }

    #[test]
    fn rejects_a_corrupt_superblock() {
        let mut b = Builder::new();
        b.add(1, "f", false, b"x");
        let img = b.build();

        // A flipped magic byte.
        let mut bad = img.clone();
        bad[1] ^= 0xFF;
        assert_eq!(Fs::open(&Image(bad)).err(), Some(FsError::BadSuperblock));

        // A flipped CRC-covered field (total_blocks) with the stored CRC left stale.
        let mut bad = img.clone();
        bad[16] ^= 0x01;
        assert_eq!(Fs::open(&Image(bad)).err(), Some(FsError::BadSuperblock));

        // A device too small to hold block 0 is an I/O error, not a bad superblock.
        assert_eq!(
            Fs::open(&Image(alloc::vec![0u8; 16])).err(),
            Some(FsError::Io)
        );

        // The intact image still opens.
        assert!(Fs::open(&Image(img)).is_ok());
    }

    #[test]
    fn a_hole_reads_as_zeros() {
        // Three full blocks of 0xAB; then punch the middle direct pointer to a hole.
        let mut b = Builder::new();
        let data = alloc::vec![0xABu8; 3 * BLOCK];
        let ino = b.add(1, "sparse.bin", false, &data);
        let mut img = b.build();

        let table = {
            let dev = Image(img.clone());
            Fs::open(&dev).unwrap().superblock().inode_table_start as usize
        };
        // Zero direct[1] of `ino` in the inode table.
        let off = table * BLOCK + ino as usize * INODE_SIZE + OFF_DIRECT + 8;
        img[off..off + 8].copy_from_slice(&0u64.to_le_bytes());

        let dev = Image(img);
        let fs = Fs::open(&dev).unwrap();
        let got = fs.read_whole(ino).unwrap();
        assert_eq!(got.len(), 3 * BLOCK);
        assert!(got[..BLOCK].iter().all(|&x| x == 0xAB), "block 0 intact");
        assert!(
            got[BLOCK..2 * BLOCK].iter().all(|&x| x == 0),
            "block 1 is a hole"
        );
        assert!(
            got[2 * BLOCK..].iter().all(|&x| x == 0xAB),
            "block 2 intact"
        );
    }

    #[test]
    fn mutations_never_panic() {
        // A valid image with a directory tree and a single-indirect file, then thousands of
        // mutated copies fed through the whole read path: nothing may panic (CLAUDE.md §72).
        let mut b = Builder::new();
        b.add(1, "hello.txt", false, b"hello");
        let etc = b.add(1, "etc", true, &[]);
        b.add(etc, "hostname", false, b"origin");
        let big: alloc::vec::Vec<u8> = (0..15 * BLOCK).map(|i| i as u8).collect();
        b.add(1, "big.bin", false, &big);
        let base = b.build();

        let mut rng = crate::testing::Rng::new(0x0516_15F5);
        for _ in 0..crate::testing::cases(4_000) {
            let mut data = base.clone();
            crate::testing::mutate(&mut data, &mut rng);
            let dev = Image(data);
            if let Ok(fs) = Fs::open(&dev) {
                for ino in 0..8u32 {
                    let _ = fs.entries(ino);
                    let _ = fs.read_whole(ino);
                    let mut buf = [0u8; 64];
                    let _ = fs.read_file(ino, 0, &mut buf);
                }
                let _ = fs.resolve("/etc/hostname");
                let _ = fs.resolve("/big.bin");
                let _ = fs.resolve("/etc/../hello.txt");
            }
        }
    }
}
