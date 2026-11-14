//! The Processor Properties Topology Table, signature `PPTT` (ACPI 6.6 §5.2.31): processor
//! hierarchy nodes and the caches private to them, linked by offsets from the table's start.
//!
//! Every reference is checked to land on the start of a structure of the right kind: a parent on a
//! processor hierarchy node, a private resource on anything but one, a next-level cache on a cache.
//! Parent chains and cache chains are followed to a bounded depth, so a cycle or an absurd depth is
//! refused rather than followed. Types other than 0 and 1 are walked and listed (ACPI 6.6 defines
//! no others).

use crate::census::Tally;
use crate::error::{Error, ErrorKind, Signature};
use crate::header::{self, Table};
use crate::read::Bytes;
use crate::walk::{Index, Raw, Shape, Walk, body};

/// The most structures a PPTT may hold here, which bounds the work of checking its references; a
/// two-socket machine of 2026 with a few hundred threads needs a few thousand. The worst case at
/// this bound is large: about 10⁹ walk steps by the independent review's estimate (its F18), for
/// nodes of 255 bytes with 58 private resources each pointing at caches 8 deep.
pub const MAX_STRUCTURES: usize = 8192;
/// The deepest processor hierarchy followed (thread, core, cluster, die, package and a few more).
pub const MAX_DEPTH: usize = 16;
/// The longest cache chain followed.
pub const MAX_CACHE_LEVELS: usize = 8;

/// A processor hierarchy node's flags (§5.2.31.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeFlags(pub u32);

impl NodeFlags {
    /// The node is a physical package's boundary (bit 0).
    #[must_use]
    pub const fn physical_package(self) -> bool {
        self.0 & 1 != 0
    }
    /// The ACPI processor ID is valid (bit 1).
    #[must_use]
    pub const fn id_valid(self) -> bool {
        self.0 & 2 != 0
    }
    /// A leaf that shares functional units with its siblings: a thread (bit 2).
    #[must_use]
    pub const fn thread(self) -> bool {
        self.0 & 4 != 0
    }
    /// A leaf (bit 3).
    #[must_use]
    pub const fn leaf(self) -> bool {
        self.0 & 8 != 0
    }
    /// Every child is the same implementation (bit 4).
    #[must_use]
    pub const fn identical(self) -> bool {
        self.0 & 16 != 0
    }
}

/// One PPTT structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PpttEntry<'a> {
    /// A processor hierarchy node (type 0).
    Node {
        /// Its flags.
        flags: NodeFlags,
        /// Its parent's offset, or 0 for a root.
        parent: u32,
        /// Its ACPI processor ID, if its flags say it is valid.
        processor_id: Option<u32>,
        /// Its private resources' offsets, four bytes each.
        resources: &'a [u8],
    },
    /// A cache (type 1).
    Cache {
        /// Which of the following fields are valid.
        flags: u32,
        /// The next level's offset, or 0.
        next_level: u32,
        /// Its size in bytes.
        size: u32,
        /// Its number of sets.
        sets: u32,
        /// Its ways.
        associativity: u8,
        /// Allocation type, cache type and write policy.
        attributes: u8,
        /// Its line size.
        line_size: u16,
        /// Its ID, if the structure is long enough to hold one and its flag says it is valid.
        cache_id: Option<u32>,
    },
    /// A type ACPI 6.6 does not define.
    Reserved(Raw<'a>),
}

fn decode<'a>(raw: &Raw<'a>, signature: Signature) -> Result<PpttEntry<'a>, Error> {
    Ok(match raw.kind {
        0 => {
            let b = body(raw, signature, 20)?;
            let flags = NodeFlags(b.u32(4)?);
            let count = usize::try_from(b.u32(16)?).unwrap_or(usize::MAX);
            if !flags.leaf() && flags.thread() {
                // Only a leaf can be a thread.
                return Err(b.error(4, ErrorKind::Inconsistent { field: "Flags" }));
            }
            PpttEntry::Node {
                flags,
                parent: b.u32(8)?,
                processor_id: if flags.id_valid() {
                    Some(b.u32(12)?)
                } else {
                    None
                },
                resources: b
                    .array_of(20, count, 4, ("Number of Private Resources", 16))?
                    .bytes(),
            }
        }
        1 => {
            // ACPI 6.6's cache structure is 28 bytes with its ID; an older one of 24 has none.
            let b = body(raw, signature, 24)?;
            let flags = b.u32(4)?;
            PpttEntry::Cache {
                flags,
                next_level: b.u32(8)?,
                size: b.u32(12)?,
                sets: b.u32(16)?,
                associativity: b.u8(20)?,
                attributes: b.u8(21)?,
                line_size: b.u16(22)?,
                cache_id: if flags & 0x80 != 0 {
                    b.opt_u32(24)
                } else {
                    None
                },
            }
        }
        _ => PpttEntry::Reserved(*raw),
    })
}

/// The checked PPTT.
#[derive(Clone, Debug)]
pub struct Pptt<'a> {
    table: Table<'a>,
    index: Index<'a>,
}

impl<'a> Pptt<'a> {
    /// Checks the table. Refused besides each structure's length: more than [`MAX_STRUCTURES`]; a
    /// thread that is not a leaf; a parent that is not a processor hierarchy node, or is a leaf; a
    /// private resource that is a processor hierarchy node or not a structure's start; a next-level
    /// cache that is not a cache; a parent chain deeper than [`MAX_DEPTH`] or a cache chain longer
    /// than [`MAX_CACHE_LEVELS`], which is how a cycle shows; and a cache ID of zero marked valid.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"PPTT")?;
        let run = table.reader().rest(header::HEADER_LEN)?;
        let index = Index::build(
            run,
            Shape::BYTE_BYTE,
            MAX_STRUCTURES,
            "more structures than the PPTT's checks take",
        )?;
        let pptt = Self { table, index };
        for raw in Walk::new(run, Shape::BYTE_BYTE) {
            let raw = raw?;
            pptt.check(&raw, decode(&raw, table.signature())?)?;
        }
        Ok(pptt)
    }

    /// How many structures.
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.count()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The structure at `offset` from the table's start, decoded, if one starts there.
    #[must_use]
    pub fn at(&self, offset: u32) -> Option<PpttEntry<'a>> {
        let raw = self.index.at(usize::try_from(offset).ok()?)?;
        decode(&raw, self.table.signature()).ok()
    }

    /// Every structure with its offset, in order.
    pub fn entries(&self) -> impl Iterator<Item = Result<(usize, PpttEntry<'a>), Error>> + 'a {
        let signature = self.table.signature();
        self.table
            .reader()
            .rest(header::HEADER_LEN)
            .ok()
            .into_iter()
            .flat_map(|run| Walk::new(run, Shape::BYTE_BYTE))
            .map(move |raw| {
                let raw = raw?;
                Ok((raw.at, decode(&raw, signature)?))
            })
    }

    fn check(&self, raw: &Raw<'a>, entry: PpttEntry<'a>) -> Result<(), Error> {
        let b = Bytes::at(raw.bytes, self.table.signature(), raw.at);
        let refuse = |at, kind| Err(b.error(at, kind));
        match entry {
            PpttEntry::Node {
                parent, resources, ..
            } => {
                // The parent chain: each a non-leaf node, ending at a root within the depth bound.
                let mut next = parent;
                let mut depth = 0usize;
                while next != 0 {
                    match self.at(next) {
                        Some(PpttEntry::Node { flags, parent, .. }) if !flags.leaf() => {
                            next = parent;
                        }
                        Some(PpttEntry::Node { .. }) => {
                            return refuse(
                                8,
                                ErrorKind::Inconsistent {
                                    field: "Parent: a leaf",
                                },
                            );
                        }
                        _ => return refuse(8, ErrorKind::Reference { field: "Parent" }),
                    }
                    depth = depth.saturating_add(1);
                    if depth > MAX_DEPTH {
                        return refuse(8, ErrorKind::Cycle);
                    }
                }
                for (i, offset) in resources.as_chunks::<4>().0.iter().enumerate() {
                    let at = i.saturating_mul(4).saturating_add(20);
                    match self.at(u32::from_le_bytes(*offset)) {
                        Some(PpttEntry::Node { .. }) | None => {
                            return refuse(
                                at,
                                ErrorKind::Reference {
                                    field: "Private Resources",
                                },
                            );
                        }
                        Some(PpttEntry::Cache { .. }) => {
                            self.cache_chain(&b, at, u32::from_le_bytes(*offset))?
                        }
                        Some(PpttEntry::Reserved(_)) => {}
                    }
                }
            }
            PpttEntry::Cache {
                next_level,
                cache_id,
                ..
            } => {
                if cache_id == Some(0) {
                    return refuse(24, ErrorKind::Value { field: "Cache ID" });
                }
                if next_level != 0 {
                    self.cache_chain(&b, 8, next_level)?;
                }
            }
            PpttEntry::Reserved(_) => {}
        }
        Ok(())
    }

    /// Follows the cache chain from `first`: each a cache, ending within the bound.
    fn cache_chain(&self, b: &Bytes<'_>, at: usize, first: u32) -> Result<(), Error> {
        let mut next = first;
        let mut levels = 0usize;
        while next != 0 {
            match self.at(next) {
                Some(PpttEntry::Cache { next_level, .. }) => next = next_level,
                _ => {
                    return Err(b.error(
                        at,
                        ErrorKind::Reference {
                            field: "Next Level of Cache",
                        },
                    ));
                }
            }
            levels = levels.saturating_add(1);
            if levels > MAX_CACHE_LEVELS {
                return Err(b.error(at, ErrorKind::Cycle));
            }
        }
        Ok(())
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(
            self.table.reader().rest(header::HEADER_LEN)?,
            Shape::BYTE_BYTE,
        ) {
            let raw = raw?;
            tally.structure(raw.kind, raw.at, raw.bytes);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::census;
    use crate::testing::table;

    /// Valid tables, for the hostile-table tests.
    pub(crate) fn samples() -> Vec<Vec<u8>> {
        vec![topology()]
    }

    /// A processor node: flags, parent, processor ID, resources.
    fn node(flags: u32, parent: u32, id: u32, resources: &[u32]) -> Vec<u8> {
        let mut n = vec![0, u8::try_from(20 + 4 * resources.len()).unwrap(), 0, 0];
        n.extend_from_slice(&flags.to_le_bytes());
        n.extend_from_slice(&parent.to_le_bytes());
        n.extend_from_slice(&id.to_le_bytes());
        n.extend_from_slice(&u32::try_from(resources.len()).unwrap().to_le_bytes());
        for r in resources {
            n.extend_from_slice(&r.to_le_bytes());
        }
        n
    }

    fn cache(next: u32, id: u32) -> Vec<u8> {
        let mut c = vec![1, 28, 0, 0];
        c.extend_from_slice(&0xFFu32.to_le_bytes());
        c.extend_from_slice(&next.to_le_bytes());
        c.extend_from_slice(&(32u32 << 10).to_le_bytes());
        c.extend_from_slice(&64u32.to_le_bytes());
        c.extend_from_slice(&[8, 0b0_01_10, 64, 0]);
        c.extend_from_slice(&id.to_le_bytes());
        c
    }

    /// A package (at 36) with an L2 (at 60), one core (at 88) with an L1 (at 112) pointing at the
    /// L2, and two threads under the core (at 140 and 160).
    fn topology() -> Vec<u8> {
        let package = node(0b1, 0, 0, &[60]);
        let l2 = cache(0, 2);
        let core = node(0, 36, 0, &[112]);
        let l1 = cache(60, 1);
        let t0 = node(0b1110, 88, 0, &[]);
        let t1 = node(0b1110, 88, 1, &[]);
        table(b"PPTT", 3, &[package, l2, core, l1, t0, t1].concat())
    }

    #[test]
    fn follows_every_reference() {
        let t = topology();
        let p = Pptt::parse(&t).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(p.len(), 6);
        assert!(matches!(p.at(88), Some(PpttEntry::Node { parent: 36, .. })));
        assert!(matches!(
            p.at(112),
            Some(PpttEntry::Cache {
                next_level: 60,
                cache_id: Some(1),
                ..
            })
        ));
        assert_eq!(p.at(85), None);
        let threads = p
            .entries()
            .filter(|e| matches!(e, Ok((_, PpttEntry::Node { flags, .. })) if flags.thread()))
            .count();
        assert_eq!(threads, 2);
        assert!(
            matches!(census::check(&t).unwrap().class, census::Class::Checked(s) if s.structures() == 6)
        );
    }

    fn refusal(structures: &[Vec<u8>]) -> ErrorKind {
        Pptt::parse(&table(b"PPTT", 3, &structures.concat()))
            .unwrap_err()
            .kind
    }

    #[test]
    fn bad_references_are_refused() {
        assert_eq!(
            refusal(&[node(0, 37, 0, &[])]),
            ErrorKind::Reference { field: "Parent" }
        );
        assert_eq!(
            refusal(&[cache(0, 1), node(0, 36, 0, &[])]),
            ErrorKind::Reference { field: "Parent" }
        );
        assert_eq!(
            refusal(&[node(0b1000, 0, 0, &[]), node(0b1000, 36, 1, &[])]),
            ErrorKind::Inconsistent {
                field: "Parent: a leaf"
            }
        );
        assert_eq!(refusal(&[node(0, 36, 0, &[])]), ErrorKind::Cycle);
        assert_eq!(
            refusal(&[node(0, 0, 0, &[36])]),
            ErrorKind::Reference {
                field: "Private Resources"
            }
        );
        assert_eq!(
            refusal(&[node(0, 0, 0, &[57]), cache(0, 1)]),
            ErrorKind::Reference {
                field: "Private Resources"
            }
        );
        assert_eq!(refusal(&[cache(36, 1)]), ErrorKind::Cycle);
        assert_eq!(
            refusal(&[cache(64, 1), node(0, 0, 0, &[])]),
            ErrorKind::Reference {
                field: "Next Level of Cache"
            }
        );
        assert_eq!(
            refusal(&[cache(0, 0)]),
            ErrorKind::Value { field: "Cache ID" }
        );
        assert_eq!(
            refusal(&[node(0b0100, 0, 0, &[])]),
            ErrorKind::Inconsistent { field: "Flags" }
        );
        let mut short = node(0, 0, 0, &[1]);
        short[1] = 23;
        short.truncate(23);
        assert!(matches!(refusal(&[short]), ErrorKind::Offset { .. }));
    }

    #[test]
    fn deep_and_many_are_bounded() {
        // A chain of MAX_DEPTH + 2 nodes, each the parent of the next.
        let mut chain = vec![node(0, 0, 0, &[])];
        for i in 1..MAX_DEPTH + 2 {
            chain.push(node(0, u32::try_from(36 + 20 * (i - 1)).unwrap(), 0, &[]));
        }
        assert!(Pptt::parse(&table(b"PPTT", 3, &chain[..MAX_DEPTH + 1].concat())).is_ok());
        assert_eq!(refusal(&chain), ErrorKind::Cycle);
        let many = vec![vec![9u8, 2]; MAX_STRUCTURES + 1];
        assert!(Pptt::parse(&table(b"PPTT", 3, &many[..MAX_STRUCTURES].concat())).is_ok());
        assert!(matches!(refusal(&many), ErrorKind::Value { .. }));
    }

    #[test]
    fn an_older_cache_structure_has_no_id() {
        let mut c = cache(0, 7);
        c[1] = 24;
        c.truncate(24);
        let t = table(b"PPTT", 2, &c);
        let p = Pptt::parse(&t).unwrap();
        assert!(matches!(
            p.at(36),
            Some(PpttEntry::Cache { cache_id: None, .. })
        ));
    }
}
