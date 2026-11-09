//! A fixed-size bit allocator (CLAUDE.md §40 allocation-bitmap maintenance; §29 resource
//! allocation). A dense bitmap of `len` bits with constant-ish allocation: `alloc` finds
//! the lowest free bit by scanning 64-bit words and picking the lowest clear bit of the
//! first non-full word. This is the structure a filesystem free-space map, a process-id
//! allocator or any small handle pool is built on. Pure, no `unsafe`; `alloc` for the word
//! vector.
//!
//! Padding bits past `len` in the final word are kept permanently set, so they are never
//! handed out and the free counter stays exact.
#![forbid(unsafe_code)]

extern crate alloc;
use alloc::vec::Vec;

/// A bitmap of `len` allocatable bits, each either free (0) or set (1).
pub struct Bitmap {
    words: Vec<u64>,
    len: usize,
    free: usize,
}

impl core::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Bitmap")
            .field("len", &self.len)
            .field("free", &self.free)
            .finish()
    }
}

impl Bitmap {
    /// A bitmap of `len` bits, all free.
    pub fn new(len: usize) -> Self {
        let nwords = len.div_ceil(64);
        let mut words = alloc::vec![0u64; nwords];
        // Mark the final word's padding bits (positions `len%64..64`) as permanently set.
        let rem = len % 64;
        if rem != 0 {
            words[nwords - 1] = !0u64 << rem;
        }
        Self {
            words,
            len,
            free: len,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The number of free bits.
    pub fn free(&self) -> usize {
        self.free
    }

    fn locate(i: usize) -> (usize, u64) {
        (i / 64, 1u64 << (i % 64))
    }

    /// Whether bit `i` is set (allocated). Out-of-range indices read as `false`.
    pub fn get(&self, i: usize) -> bool {
        if i >= self.len {
            return false;
        }
        let (w, b) = Self::locate(i);
        self.words[w] & b != 0
    }

    /// Sets bit `i`; returns whether it changed from free to set (so a caller can tell a
    /// fresh allocation from a double-set). Out-of-range is a no-op returning `false`.
    pub fn set(&mut self, i: usize) -> bool {
        if i >= self.len {
            return false;
        }
        let (w, b) = Self::locate(i);
        if self.words[w] & b == 0 {
            self.words[w] |= b;
            self.free -= 1;
            true
        } else {
            false
        }
    }

    /// Clears bit `i`; returns whether it changed from set to free (so freeing an already-
    /// free bit, a double-free, is reported as `false`). Out-of-range is a no-op.
    pub fn clear(&mut self, i: usize) -> bool {
        if i >= self.len {
            return false;
        }
        let (w, b) = Self::locate(i);
        if self.words[w] & b != 0 {
            self.words[w] &= !b;
            self.free += 1;
            true
        } else {
            false
        }
    }

    /// Allocates the lowest free bit and returns its index, or `None` when full.
    pub fn alloc(&mut self) -> Option<usize> {
        if self.free == 0 {
            return None;
        }
        for (wi, word) in self.words.iter_mut().enumerate() {
            if *word != u64::MAX {
                let b = (!*word).trailing_zeros() as usize;
                *word |= 1u64 << b;
                self.free -= 1;
                return Some(wi * 64 + b);
            }
        }
        None // unreachable while `free > 0`, but defensive
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn alloc_returns_the_lowest_free_bit() {
        let mut b = Bitmap::new(10);
        assert_eq!(b.free(), 10);
        assert_eq!(b.alloc(), Some(0));
        assert_eq!(b.alloc(), Some(1));
        assert_eq!(b.alloc(), Some(2));
        assert_eq!(b.free(), 7);
        // Free bit 1 and the next alloc reuses the lowest free slot.
        assert!(b.clear(1));
        assert!(!b.clear(1), "a double free is reported");
        assert_eq!(b.alloc(), Some(1));
        assert_eq!(b.free(), 7);
    }

    #[test]
    fn fills_exactly_len_bits_including_a_partial_word() {
        // 130 bits spans two full words and a 2-bit third word.
        let mut b = Bitmap::new(130);
        let mut seen = Vec::new();
        while let Some(i) = b.alloc() {
            seen.push(i);
        }
        assert_eq!(
            seen.len(),
            130,
            "exactly len bits, no padding bits handed out"
        );
        assert_eq!(b.free(), 0);
        assert_eq!(b.alloc(), None, "full");
        // Every index 0..130 was allocated exactly once.
        seen.sort_unstable();
        assert!(seen.iter().copied().eq(0..130));
        // No index at or past len is ever set.
        assert!(!b.get(130) && !b.get(200));
    }

    #[test]
    fn set_and_get_track_the_free_count() {
        let mut b = Bitmap::new(64);
        assert!(b.set(5));
        assert!(!b.set(5), "a double set is reported");
        assert!(b.get(5));
        assert_eq!(b.free(), 63);
        assert_eq!(b.alloc(), Some(0), "5 is taken, 0 is still the lowest free");
        assert!(!b.get(1000), "out of range reads false");
        assert!(!b.set(1000) && !b.clear(1000), "out of range is a no-op");
    }

    /// Model-based test: random alloc/free/set/clear against a `Vec<bool>` oracle. The
    /// bitmap's `get`, `free` count and lowest-free allocation must match throughout.
    #[test]
    fn matches_a_vec_model() {
        const N: usize = 200; // spans three words with a partial last one
        let mut b = Bitmap::new(N);
        let mut used = [false; N];
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };

        for _ in 0..60_000 {
            match next() % 4 {
                0 => {
                    let got = b.alloc();
                    let expect = (0..N).find(|&i| !used[i]);
                    assert_eq!(got, expect, "alloc must return the lowest free index");
                    if let Some(i) = got {
                        used[i] = true;
                    }
                }
                1 => {
                    let i = (next() as usize) % N;
                    assert_eq!(b.clear(i), used[i]);
                    used[i] = false;
                }
                2 => {
                    let i = (next() as usize) % N;
                    assert_eq!(b.set(i), !used[i]);
                    used[i] = true;
                }
                _ => {
                    let i = (next() as usize) % N;
                    assert_eq!(b.get(i), used[i]);
                }
            }
            assert_eq!(b.free(), used.iter().filter(|&&u| !u).count());
        }
    }
}
