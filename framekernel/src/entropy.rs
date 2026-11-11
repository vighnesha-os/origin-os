//! Hardware entropy with bounded retries (CLAUDE.md §59 secure randomness). The x86
//! `RDRAND`/`RDSEED` instructions can transiently fail (they clear the carry flag) when the
//! on-chip entropy pool is momentarily drained; Intel's guidance is a short bounded retry
//! loop, not an unbounded spin. This module holds that retry and buffer-fill logic as pure
//! functions over an abstract one-shot [`Source`], so it is tested on the host; the kernel
//! supplies the real instruction (`kernel.rs`/`cpu.rs`).
//!
//! This is a *true-random* source, not a deterministic RBG: it returns hardware bits or
//! fails. A caller that wants a reseedable stream builds a DRBG over it — a later, reviewed
//! piece (§59 asks for known-answer tests and independent review), not invented here.

/// A single hardware entropy attempt.
pub trait Source {
    /// One attempt: `Some(bits)` when the instruction succeeded (carry set), `None` on a
    /// transient failure that a retry may satisfy.
    fn try_next(&mut self) -> Option<u64>;
}

/// The retry budget Intel documents for `RDRAND` (ten retries, i.e. eleven attempts).
pub const RDRAND_RETRIES: u32 = 10;

/// Draws one 64-bit value, retrying a transiently-failing `source` up to `retries` extra
/// times (`retries + 1` attempts in all). Returns `None` only if every attempt failed, which
/// a caller must treat as "no entropy available", never as the value zero.
pub fn draw<S: Source + ?Sized>(source: &mut S, retries: u32) -> Option<u64> {
    for _ in 0..=retries {
        if let Some(v) = source.try_next() {
            return Some(v);
        }
    }
    None
}

/// Fills `out` with entropy, drawing 64-bit words (each with up to `retries` retries) and
/// taking the low bytes of the last word for a partial final chunk. Returns whether the
/// whole buffer was filled; on `false` the buffer is only partly written and must not be
/// used.
pub fn fill<S: Source + ?Sized>(source: &mut S, retries: u32, out: &mut [u8]) -> bool {
    for chunk in out.chunks_mut(8) {
        let Some(v) = draw(source, retries) else {
            return false;
        };
        let bytes = v.to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    true
}

/// 64 bits of this processor's hardware entropy: RDSEED, else RDRAND, each attempt with
/// [`draw`]'s bounded retries. `None` when the processor gives none, which a caller must treat
/// as "no entropy", never as a value: it stops rather than use something predictable. For an
/// identifier an off-path sender must not guess (a DHCP transaction ID, a TFTP port) and for
/// where a process's start record goes (ADR-0005 C4).
#[cfg(target_os = "none")]
pub(crate) fn hardware_u64() -> Option<u64> {
    use crate::features::Features;

    struct Hardware(Features);
    impl Source for Hardware {
        fn try_next(&mut self) -> Option<u64> {
            if self.0.rdseed
                && let Some(v) = crate::cpu::rdseed()
            {
                return Some(v);
            }
            if self.0.rdrand {
                crate::cpu::rdrand()
            } else {
                None
            }
        }
    }

    let features = Features::decode(&crate::cpu::gather_cpuid());
    draw(&mut Hardware(features), RDRAND_RETRIES)
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// A source driven by a script of attempt outcomes; records how many attempts it saw.
    struct Scripted {
        outcomes: Vec<Option<u64>>,
        at: usize,
        attempts: usize,
    }
    impl Scripted {
        fn new(outcomes: Vec<Option<u64>>) -> Self {
            Self {
                outcomes,
                at: 0,
                attempts: 0,
            }
        }
    }
    impl Source for Scripted {
        fn try_next(&mut self) -> Option<u64> {
            self.attempts += 1;
            let v = self.outcomes.get(self.at).copied().flatten();
            self.at += 1;
            v
        }
    }

    #[test]
    fn draw_retries_until_success() {
        // Two transient failures then a value: four retries is enough, one is not.
        let mut s = Scripted::new(vec![None, None, Some(0xABCD)]);
        assert_eq!(draw(&mut s, 4), Some(0xABCD));
        assert_eq!(s.attempts, 3, "stopped at the first success");

        let mut s = Scripted::new(vec![None, None, Some(0xABCD)]);
        assert_eq!(draw(&mut s, 1), None, "only two attempts, both failed");
        assert_eq!(s.attempts, 2, "retries + 1 attempts");
    }

    #[test]
    fn draw_gives_up_after_the_budget() {
        let mut s = Scripted::new(vec![None, None, None, None]);
        assert_eq!(draw(&mut s, 2), None);
        assert_eq!(s.attempts, 3, "exactly retries + 1 attempts, then None");
    }

    #[test]
    fn fill_handles_a_partial_final_chunk() {
        // 20 bytes = two full u64 words + a 4-byte tail from a third word's low bytes.
        let words = vec![
            Some(0x0807_0605_0403_0201u64),
            Some(0x100F_0E0D_0C0B_0A09u64),
            Some(0x1817_1615_1413_1211u64),
        ];
        let mut s = Scripted::new(words);
        let mut out = [0u8; 20];
        assert!(fill(&mut s, 0, &mut out));
        assert_eq!(&out[0..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&out[8..16], &[9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(
            &out[16..20],
            &[17, 18, 19, 20],
            "low 4 bytes of the third word"
        );
        assert_eq!(s.attempts, 3, "one draw per 8-byte chunk");
    }

    #[test]
    fn fill_fails_without_mutating_usefully_when_the_source_dies() {
        let mut s = Scripted::new(vec![Some(1), None, None]);
        let mut out = [0u8; 16];
        assert!(!fill(&mut s, 1, &mut out), "the second word never arrives");
    }

    #[test]
    fn empty_fill_always_succeeds() {
        let mut s = Scripted::new(vec![]);
        assert!(fill(&mut s, 0, &mut []));
        assert_eq!(s.attempts, 0);
    }
}
