//! What a painted stack shows once a closure has run on it (TASKS item 68): the scan behind
//! `stack::with_measured_stack`, kept apart so that the host tests run it. It reads words through
//! a function and holds no memory of its own, so it needs no `unsafe`.
//!
//! A stack is painted with [`PAINT`] in every 8-byte word before the closure runs. Afterwards the
//! lowest word that no longer holds it is the deepest the closure (or anything that ran on its
//! stack) wrote, and the depth is the distance from the stack's top down to that word.

/// The pattern written over every word of a measured stack. It is neither a canonical address
/// (its bits 63 to 47 are not all equal, so no pointer the kernel keeps is ever this value) nor a
/// small integer, so a word the closure writes is unlikely to hold it by chance. A word that does
/// is not counted (the coordinating session's ruling on TASKS item 68, Q2).
pub const PAINT: u64 = 0xB4D5_7AC6_9E1F_83E7;

/// The depth, in bytes, of a stack of `words` 8-byte words, the lowest first, once something has
/// run on it: from the top down to the lowest word that no longer holds [`PAINT`], that word
/// included. 0 if every word still holds it.
///
/// `word(i)` reads word `i`, `0 <= i < words`, word 0 being the lowest. The scan goes up from the
/// bottom and stops at the first changed word, so a painted word above it (a hole the closure
/// reserved and never wrote) does not shorten the depth.
pub fn depth(words: usize, mut word: impl FnMut(usize) -> u64) -> usize {
    (0..words)
        .find(|&i| word(i) != PAINT)
        .map_or(0, |lowest| (words - lowest) * 8)
}

#[cfg(test)]
mod tests {
    use super::{PAINT, depth};

    /// A stack of `words` painted words with each of `written` changed.
    fn stack(words: usize, written: &[usize]) -> Vec<u64> {
        let mut s = vec![PAINT; words];
        for &i in written {
            s[i] = 0;
        }
        s
    }

    fn measure(s: &[u64]) -> usize {
        depth(s.len(), |i| s[i])
    }

    #[test]
    fn the_paint_is_no_canonical_address_and_no_small_integer() {
        // Canonical in 4-level paging: bits 63 to 47 all equal.
        let high = PAINT >> 47;
        assert!(
            high != 0 && high != (1 << 17) - 1,
            "{PAINT:#x} is canonical"
        );
        assert!(PAINT > u64::from(u32::MAX));
        assert!(
            PAINT.wrapping_neg() > u64::from(u32::MAX),
            "nor a small negative one"
        );
    }

    #[test]
    fn no_word_changed_is_no_depth() {
        assert_eq!(measure(&stack(512, &[])), 0);
    }

    #[test]
    fn the_bottom_word_changed_is_the_whole_stack() {
        assert_eq!(measure(&stack(512, &[0])), 4096);
    }

    #[test]
    fn the_deepest_change_wins_over_a_hole_above_it() {
        // Words 100 to 499 untouched, as a frame that reserved them and wrote none would leave
        // them; the top 12 words and word 40 written.
        let mut written: Vec<usize> = (500..512).collect();
        written.push(40);
        assert_eq!(measure(&stack(512, &written)), (512 - 40) * 8);
    }

    #[test]
    fn each_position_gives_its_own_depth() {
        for i in 0..64 {
            assert_eq!(measure(&stack(64, &[i])), (64 - i) * 8, "word {i}");
        }
    }

    #[test]
    fn a_stack_of_no_words_has_no_depth() {
        assert_eq!(depth(0, |_| unreachable!("no word is read")), 0);
    }

    #[test]
    fn a_word_written_with_the_paint_is_not_counted() {
        // What the depth misses (Q2): the closure wrote the paint's own value at word 3.
        let mut s = stack(16, &[10]);
        s[3] = PAINT;
        assert_eq!(measure(&s), (16 - 10) * 8);
    }
}
