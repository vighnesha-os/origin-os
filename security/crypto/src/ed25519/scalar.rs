//! Scalars modulo the group order L = 2^252 + 27742317777372353535851937790883648493
//! (RFC 8032 §5.1), as four little-endian 64-bit words.

use super::constants::L;

/// Whether a < b, for little-endian words.
fn less_than(a: &[u64; 4], b: &[u64; 4]) -> bool {
    for (x, y) in a.iter().rev().zip(b.iter().rev()) {
        if x != y {
            return x < y;
        }
    }
    false
}

/// a − b with the borrow discarded; the caller guarantees a ≥ b.
fn sub(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let mut out = *a;
    let mut borrow = false;
    for (x, y) in out.iter_mut().zip(b) {
        let (d1, b1) = x.overflowing_sub(*y);
        let (d2, b2) = d1.overflowing_sub(u64::from(borrow));
        *x = d2;
        borrow = b1 | b2;
    }
    out
}

fn words(bytes: &[u8; 32]) -> [u64; 4] {
    let mut w = [0u64; 4];
    for (word, chunk) in w.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    w
}

/// S from a signature, if it is canonical: S < L (RFC 8032 §5.1.7 step 1).
pub(crate) fn canonical(bytes: &[u8; 32]) -> Option<[u64; 4]> {
    let s = words(bytes);
    less_than(&s, &L).then_some(s)
}

/// A 512-bit little-endian integer (a SHA-512 digest) reduced mod L, one bit at a time from
/// the top: r ← 2r + bit, minus L if that reaches L. Since r < L before each step,
/// 2r + 1 < 2L < 2^254 fits the four words and one subtraction suffices. Variable time;
/// verification hashes public data only.
pub(crate) fn reduce_wide(bytes: &[u8; 64]) -> [u64; 4] {
    let mut r = [0u64; 4];
    for &byte in bytes.iter().rev() {
        for bit in (0..8u32).rev() {
            let mut carry = u64::from(byte.wrapping_shr(bit) & 1);
            for w in r.iter_mut() {
                let top = *w >> 63;
                *w = (*w << 1) | carry;
                carry = top;
            }
            if !less_than(&r, &L) {
                r = sub(&r, &L);
            }
        }
    }
    r
}
