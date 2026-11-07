//! A string-keyed hash map (CLAUDE.md §29 resource tables, §30 handle lookup). Open
//! addressing with linear probing and tombstone deletion, keyed by owned `String` and
//! hashed with FNV-1a (64-bit). The kernel's name→object tables (a VFS directory, a handle
//! or service registry, a small cache) want an O(1)-expected map without a third-party
//! crate; this is it. It grows (never shrinks) when the load factor passes ~70%. Pure, no
//! `unsafe`; `alloc` for the buckets.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

/// FNV-1a (64-bit) of a byte string (offset basis and prime from the FNV specification).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[derive(Clone)]
enum Slot<V> {
    Empty,
    Tombstone,
    Full(String, V),
}

/// A map from `String` keys to `V` values.
pub struct StrMap<V> {
    slots: Vec<Slot<V>>,
    len: usize,      // live entries
    occupied: usize, // live + tombstones (probe-sequence length driver)
}

impl<V> StrMap<V> {
    /// An empty map.
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            len: 0,
            occupied: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bucket index a key hashes to, given a capacity (a power of two).
    fn index(cap: usize, key: &str) -> usize {
        (fnv1a(key.as_bytes()) as usize) & (cap - 1)
    }

    /// Inserts `key`→`value`, returning the previous value if the key was present.
    pub fn insert(&mut self, key: String, value: V) -> Option<V> {
        // Grow when live+tombstones would exceed ~70% of capacity.
        if (self.occupied + 1) * 10 >= self.slots.len().max(1) * 7 {
            self.grow();
        }
        let cap = self.slots.len();
        let mut i = Self::index(cap, &key);
        let mut first_tomb: Option<usize> = None;
        loop {
            match &self.slots[i] {
                Slot::Empty => {
                    let at = first_tomb.unwrap_or(i);
                    if first_tomb.is_none() {
                        self.occupied += 1;
                    }
                    self.slots[at] = Slot::Full(key, value);
                    self.len += 1;
                    return None;
                }
                Slot::Tombstone => {
                    first_tomb.get_or_insert(i);
                }
                Slot::Full(k, _) if *k == key => {
                    let old = core::mem::replace(&mut self.slots[i], Slot::Full(key, value));
                    return match old {
                        Slot::Full(_, v) => Some(v),
                        _ => None,
                    };
                }
                Slot::Full(..) => {}
            }
            i = (i + 1) & (cap - 1);
        }
    }

    /// Finds the slot index holding `key`, if present.
    fn find(&self, key: &str) -> Option<usize> {
        if self.slots.is_empty() {
            return None;
        }
        let cap = self.slots.len();
        let mut i = Self::index(cap, key);
        loop {
            match &self.slots[i] {
                Slot::Empty => return None,
                Slot::Full(k, _) if k == key => return Some(i),
                _ => {}
            }
            i = (i + 1) & (cap - 1);
        }
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.find(key).map(|i| match &self.slots[i] {
            Slot::Full(_, v) => v,
            _ => unreachable!(),
        })
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.find(key).is_some()
    }

    /// Removes `key`, returning its value if it was present. The slot becomes a tombstone.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        let i = self.find(key)?;
        self.len -= 1;
        match core::mem::replace(&mut self.slots[i], Slot::Tombstone) {
            Slot::Full(_, v) => Some(v),
            _ => None,
        }
    }

    /// Doubles the capacity (from a small base) and rehashes live entries, dropping
    /// tombstones.
    fn grow(&mut self) {
        let new_cap = (self.slots.len() * 2).max(8);
        let old = core::mem::take(&mut self.slots);
        self.slots.resize_with(new_cap, || Slot::Empty);
        self.len = 0;
        self.occupied = 0;
        for slot in old {
            if let Slot::Full(k, v) = slot {
                self.insert(k, v);
            }
        }
    }

    /// Visits every `(key, value)` (unspecified order).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.slots.iter().filter_map(|s| match s {
            Slot::Full(k, v) => Some((k.as_str(), v)),
            _ => None,
        })
    }
}

impl<V> Default for StrMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> core::fmt::Debug for StrMap<V> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StrMap")
            .field("len", &self.len)
            .field("capacity", &self.slots.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::format;

    #[test]
    fn fnv1a_known_values() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn basic_insert_get_remove() {
        let mut m = StrMap::new();
        assert!(m.is_empty());
        assert_eq!(m.insert("one".into(), 1), None);
        assert_eq!(m.insert("two".into(), 2), None);
        assert_eq!(
            m.insert("one".into(), 11),
            Some(1),
            "replace returns the old value"
        );
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("one"), Some(&11));
        assert_eq!(m.get("two"), Some(&2));
        assert_eq!(m.get("three"), None);
        assert!(m.contains_key("one") && !m.contains_key("x"));
        assert_eq!(m.remove("one"), Some(11));
        assert_eq!(m.remove("one"), None, "already removed");
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("one"), None);
        assert_eq!(
            m.get("two"),
            Some(&2),
            "a tombstone does not hide a later key"
        );
    }

    #[test]
    fn tombstone_reuse_and_growth() {
        let mut m = StrMap::new();
        // Insert many, remove many, insert many — exercising tombstones and several grows.
        for i in 0..500 {
            m.insert(format!("key{i}"), i);
        }
        assert_eq!(m.len(), 500);
        for i in 0..250 {
            assert_eq!(m.remove(&format!("key{i}")), Some(i));
        }
        assert_eq!(m.len(), 250);
        for i in 0..250 {
            m.insert(format!("new{i}"), i + 1000);
        }
        assert_eq!(m.len(), 500);
        for i in 250..500 {
            assert_eq!(m.get(&format!("key{i}")), Some(&i));
        }
        for i in 0..250 {
            assert_eq!(m.get(&format!("new{i}")), Some(&(i + 1000)));
            assert_eq!(m.get(&format!("key{i}")), None);
        }
    }

    /// Model-based test: random operations must track a `BTreeMap` oracle exactly.
    #[test]
    fn matches_a_btreemap_model() {
        let mut m = StrMap::new();
        let mut model: BTreeMap<String, u32> = BTreeMap::new();
        let mut x = 0x1234_5678_9abc_def1u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..50_000 {
            let key = format!("k{}", next() % 64); // a small key space → many collisions
            match next() % 3 {
                0 => {
                    let v = next() as u32;
                    assert_eq!(m.insert(key.clone(), v), model.insert(key, v));
                }
                1 => assert_eq!(m.remove(&key), model.remove(&key)),
                _ => assert_eq!(m.get(&key), model.get(&key)),
            }
            assert_eq!(m.len(), model.len());
        }
        // Every live entry agrees, both directions.
        for (k, v) in m.iter() {
            assert_eq!(model.get(k), Some(v));
        }
        assert_eq!(m.len(), model.len());
    }
}
