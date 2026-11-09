//! An IPv4 longest-prefix-match routing table (CLAUDE.md §54 routing). Routes are held in a
//! binary (bit) trie over the 32-bit address, so a lookup walks at most 32 nodes and always
//! returns the most specific matching route — a `/0` default route, if present, being the
//! least specific. The stored value is generic (`V`), so a caller keeps whatever a route
//! needs (a gateway and an interface). Pure, no `unsafe`; `alloc` for the node pool.
//!
//! The trie is the usual LPM structure: inserting a prefix of length *n* walks the top *n*
//! address bits (creating nodes as needed) and marks the final node; a lookup walks the
//! address bits and remembers the deepest marked node it passes. Re-inserting a prefix
//! replaces its value.

extern crate alloc;
use alloc::vec::Vec;

use crate::net::Ipv4;

/// An IPv4 address as a big-endian `u32` (octet 0 is the most significant byte).
fn to_u32(ip: Ipv4) -> u32 {
    u32::from_be_bytes(ip.0)
}

struct Node<V> {
    children: [Option<usize>; 2],
    value: Option<V>,
}

/// A longest-prefix-match table from IPv4 prefixes to values of type `V`.
pub struct Table<V: Copy> {
    nodes: Vec<Node<V>>,
}

impl<V: Copy> Table<V> {
    /// An empty table (not even a default route).
    pub fn new() -> Self {
        Self {
            nodes: alloc::vec![Node {
                children: [None, None],
                value: None,
            }],
        }
    }

    /// Adds or replaces the route for `prefix`/`prefix_len` (a length above 32 is clamped;
    /// host bits beyond the length are ignored). `prefix_len` 0 is the default route.
    pub fn insert(&mut self, prefix: Ipv4, prefix_len: u8, value: V) {
        let len = prefix_len.min(32);
        let bits = to_u32(prefix);
        let mut cur = 0usize;
        for i in 0..len {
            let bit = ((bits >> (31 - i)) & 1) as usize;
            cur = match self.nodes[cur].children[bit] {
                Some(c) => c,
                None => {
                    let c = self.nodes.len();
                    self.nodes.push(Node {
                        children: [None, None],
                        value: None,
                    });
                    self.nodes[cur].children[bit] = Some(c);
                    c
                }
            };
        }
        self.nodes[cur].value = Some(value);
    }

    /// The value of the most specific route that matches `addr`, or `None` if none does.
    pub fn lookup(&self, addr: Ipv4) -> Option<V> {
        let bits = to_u32(addr);
        let mut cur = 0usize;
        let mut best = self.nodes[0].value; // the default route, if any
        for i in 0..32 {
            let bit = ((bits >> (31 - i)) & 1) as usize;
            match self.nodes[cur].children[bit] {
                Some(c) => {
                    cur = c;
                    if let Some(v) = self.nodes[c].value {
                        best = Some(v);
                    }
                }
                None => break,
            }
        }
        best
    }
}

impl<V: Copy> Default for Table<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Copy> core::fmt::Debug for Table<V> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("route::Table")
            .field("nodes", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> Ipv4 {
        Ipv4([a, b, c, d])
    }

    #[test]
    fn most_specific_route_wins() {
        let mut t = Table::new();
        t.insert(ip(0, 0, 0, 0), 0, 1); // default
        t.insert(ip(10, 0, 0, 0), 8, 2); // 10.0.0.0/8
        t.insert(ip(10, 1, 0, 0), 16, 3); // 10.1.0.0/16
        t.insert(ip(10, 1, 2, 0), 24, 4); // 10.1.2.0/24

        assert_eq!(t.lookup(ip(8, 8, 8, 8)), Some(1), "default route");
        assert_eq!(t.lookup(ip(10, 2, 3, 4)), Some(2), "/8");
        assert_eq!(t.lookup(ip(10, 1, 9, 9)), Some(3), "/16 beats /8");
        assert_eq!(t.lookup(ip(10, 1, 2, 200)), Some(4), "/24 is most specific");
    }

    #[test]
    fn no_default_means_no_match() {
        let mut t = Table::new();
        t.insert(ip(192, 168, 0, 0), 16, 42);
        assert_eq!(t.lookup(ip(192, 168, 5, 5)), Some(42));
        assert_eq!(t.lookup(ip(172, 16, 0, 1)), None, "no default, no match");
    }

    #[test]
    fn host_bits_ignored_and_reinsert_replaces() {
        let mut t = Table::new();
        // Host bits beyond the length do not matter.
        t.insert(ip(10, 0, 0, 7), 8, 1);
        assert_eq!(t.lookup(ip(10, 255, 255, 255)), Some(1));
        // Re-inserting the same prefix replaces the value.
        t.insert(ip(10, 0, 0, 0), 8, 9);
        assert_eq!(t.lookup(ip(10, 1, 1, 1)), Some(9));
        // A /32 host route.
        t.insert(ip(10, 0, 0, 1), 32, 5);
        assert_eq!(t.lookup(ip(10, 0, 0, 1)), Some(5));
        assert_eq!(t.lookup(ip(10, 0, 0, 2)), Some(9), "falls back to /8");
    }

    /// The brute-force reference: scan every route, keep the longest prefix that matches,
    /// last insertion winning a tie (which only arises for the same prefix).
    fn brute(routes: &[(u32, u8, u32)], addr: u32) -> Option<u32> {
        let mut best: Option<(u8, u32)> = None;
        for &(prefix, len, value) in routes {
            let matches = len == 0 || {
                let shift = 32 - u32::from(len);
                (addr & (!0u32 << shift)) == (prefix & (!0u32 << shift))
            };
            if matches && best.is_none_or(|(bl, _)| len >= bl) {
                best = Some((len, value));
            }
        }
        best.map(|(_, v)| v)
    }

    #[test]
    fn matches_brute_force_over_random_tables() {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };

        for _ in 0..2_000 {
            let mut t = Table::new();
            let mut routes: Vec<(u32, u8, u32)> = Vec::new();
            let count = (next() % 24) as u32;
            for value in 0..count {
                let len = (next() % 33) as u8;
                // Prefixes drawn from a small space so collisions and nesting are common.
                let prefix = (next() as u32) & 0xF0FF_00FF;
                routes.push((prefix, len, value));
                t.insert(Ipv4(prefix.to_be_bytes()), len, value);
            }
            for _ in 0..30 {
                let addr = (next() as u32) & 0xF0FF_00FF;
                assert_eq!(
                    t.lookup(Ipv4(addr.to_be_bytes())),
                    brute(&routes, addr),
                    "addr {addr:#010x}, routes {routes:?}"
                );
            }
        }
    }
}
