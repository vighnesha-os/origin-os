//! Frame-pointer stack unwinding (CLAUDE.md §69: kernel panic diagnostics, crash dumps).
//!
//! Origin's freestanding images are built with frame pointers (`-C
//! force-frame-pointers=yes`, CLAUDE.md §14), so each function's prologue pushes the
//! caller's RBP and sets RBP to that saved slot. A frame is then two words at RBP: `[RBP]`
//! is the caller's RBP and `[RBP + 8]` is the return address. [`walk`] follows that chain.
//!
//! The chain is untrusted (a crash may have corrupted the stack), so the walk reads each
//! word through a caller-supplied accessor that returns `None` for an address it will not
//! read, and it stops at a null, unaligned or non-increasing RBP. This keeps it pure and
//! testable on the host; the kernel supplies an accessor bounded to the running stack.

/// Walks the frame-pointer chain starting at `rbp`, writing each return address into `out`
/// (innermost caller first) and returning how many it found. `read(addr)` returns the
/// 8-byte word at `addr`, or `None` to stop the walk.
///
/// The walk stops at `out.len()` frames, or when a frame pointer is null, not 8-aligned,
/// or not strictly greater than the previous one by at most `max_frame` bytes (stacks grow
/// down, so each saved RBP is above the current one; the bound rejects a wild pointer).
pub fn walk(
    mut rbp: u64,
    max_frame: u64,
    mut read: impl FnMut(u64) -> Option<u64>,
    out: &mut [u64],
) -> usize {
    let mut n = 0;
    while n < out.len() {
        if rbp == 0 || rbp & 7 != 0 {
            break;
        }
        let Some(saved_rbp) = read(rbp) else { break };
        let Some(return_addr) = read(rbp.wrapping_add(8)) else {
            break;
        };
        if return_addr == 0 {
            break;
        }
        if let Some(slot) = out.get_mut(n) {
            *slot = return_addr;
        }
        n += 1;
        // The next frame must be above this one, by a sane amount.
        if saved_rbp <= rbp || saved_rbp - rbp > max_frame {
            break;
        }
        rbp = saved_rbp;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::collections::BTreeMap;

    /// A synthetic stack: a map from address to word, and a base RBP.
    struct Stack {
        words: BTreeMap<u64, u64>,
    }

    impl Stack {
        /// Builds frames from `(rbp, return_addr)` pairs, innermost first, each linking to
        /// the next; returns the stack and the innermost RBP.
        fn build(frames: &[(u64, u64)]) -> (Self, u64) {
            let mut words = BTreeMap::new();
            for (i, &(rbp, ret)) in frames.iter().enumerate() {
                let next_rbp = frames.get(i + 1).map_or(0, |&(r, _)| r);
                words.insert(rbp, next_rbp);
                words.insert(rbp + 8, ret);
            }
            let base = frames.first().map_or(0, |&(r, _)| r);
            (Self { words }, base)
        }

        fn read(&self, addr: u64) -> Option<u64> {
            self.words.get(&addr).copied()
        }
    }

    #[test]
    fn a_clean_chain_yields_every_return_address() {
        let (s, rbp) = Stack::build(&[(0x1000, 0xAAAA), (0x1100, 0xBBBB), (0x1200, 0xCCCC)]);
        let mut out = [0u64; 8];
        let n = walk(rbp, 1 << 20, |a| s.read(a), &mut out);
        assert_eq!(&out[..n], &[0xAAAA, 0xBBBB, 0xCCCC]);
    }

    #[test]
    fn the_walk_stops_at_the_chain_end() {
        // The last frame's saved RBP is 0 (the entry stub), so the walk ends after it.
        let (s, rbp) = Stack::build(&[(0x2000, 0x11), (0x2080, 0x22)]);
        let mut out = [0u64; 8];
        assert_eq!(walk(rbp, 1 << 20, |a| s.read(a), &mut out), 2);
    }

    #[test]
    fn capacity_bounds_the_output() {
        let frames: std::vec::Vec<(u64, u64)> =
            (0..20).map(|i| (0x1000 + i * 0x100, 0x5000 + i)).collect();
        let (s, rbp) = Stack::build(&frames);
        let mut out = [0u64; 4];
        assert_eq!(walk(rbp, 1 << 20, |a| s.read(a), &mut out), 4);
        assert_eq!(out, [0x5000, 0x5001, 0x5002, 0x5003]);
    }

    #[test]
    fn a_wild_frame_pointer_stops_the_walk() {
        // A non-increasing RBP (the second frame points back down): one address, then stop.
        let (s, _) = Stack::build(&[]);
        let mut words = s.words;
        words.insert(0x3000, 0x2000); // saved RBP below the current one
        words.insert(0x3008, 0xDEAD);
        let s = Stack { words };
        let mut out = [0u64; 8];
        let n = walk(0x3000, 1 << 20, |a| s.read(a), &mut out);
        assert_eq!(
            &out[..n],
            &[0xDEAD],
            "took the return address, then stopped"
        );

        // An unaligned starting RBP yields nothing.
        assert_eq!(walk(0x3001, 1 << 20, |a| s.read(a), &mut out), 0);
        // A frame too far above the previous one stops the walk.
        let (s, rbp) = Stack::build(&[(0x4000, 0x1), (0x4000 + (2 << 20), 0x2)]);
        assert_eq!(walk(rbp, 1 << 20, |a| s.read(a), &mut out), 1);
    }

    #[test]
    fn an_unreadable_word_stops_the_walk_without_panicking() {
        // `read` returns None for the saved RBP of the second frame.
        let mut words = BTreeMap::new();
        words.insert(0x5000u64, 0x5100u64);
        words.insert(0x5008, 0xF00D);
        // 0x5100 and 0x5108 are absent, so reading them returns None.
        let s = Stack { words };
        let mut out = [0u64; 8];
        let n = walk(0x5000, 1 << 20, |a| s.read(a), &mut out);
        assert_eq!(&out[..n], &[0xF00D]);
    }
}
