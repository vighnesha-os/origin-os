//! Keccak-f[1600], the permutation under SHA-3 and SHAKE (FIPS 202 §3).
//!
//! The state is 25 lanes of 64 bits. Lane (x, y) is `state[x + 5 * y]`, and bit z of a lane
//! is bit z of its `u64`: §3.1.2 maps string bit 64(5y + x) + z to A[x, y, z], and Appendix
//! B.1 orders the bits of each byte least significant first, so a lane is the little-endian
//! `u64` of its eight bytes.
//!
//! Every constant is computed at compile time from its definition: the round constants from
//! the LFSR of Algorithm 5, placed per Algorithm 6; the ρ offsets from the (x, y) walk of
//! Algorithm 2; and the π mapping from Algorithm 3. No table is typed in. `src/tests_sha3.rs`
//! re-derives all three a second way.
//!
//! The permutation reads and writes lanes at fixed positions only, so neither its branches
//! nor its memory accesses depend on the state.

/// ℓ, where the lane width is w = 2^ℓ = 64 bits (§3.1, Table 1).
const L: u32 = 6;
/// Lanes in the state: b / w = 1600 / 64 (§3.1).
const LANES: usize = 25;
/// Rounds of Keccak-f[1600] = Keccak-p[1600, 12 + 2ℓ] (§3.4).
const ROUNDS: usize = 24;

/// The ι round constant of each round i_r = 0..=23, in order (Algorithm 6).
pub(crate) const ROUND_CONSTANTS: [u64; ROUNDS] = {
    let mut table = [0; ROUNDS];
    // Slice patterns, not indexing: the crate's no-panic lints apply to const code too.
    let mut rest: &mut [u64] = &mut table;
    let mut round = 0;
    while let [slot, tail @ ..] = rest {
        *slot = round_constant(round);
        rest = tail;
        round = round.wrapping_add(1);
    }
    table
};

/// The ρ rotation of lane `x + 5y`, in bits (Algorithm 2).
pub(crate) const RHO: [u32; LANES] = {
    let mut table = [0; LANES];
    let mut rest: &mut [u32] = &mut table;
    let mut lane = 0;
    while let [slot, tail @ ..] = rest {
        *slot = rho_offset(lane % 5, lane / 5);
        rest = tail;
        lane = lane.wrapping_add(1);
    }
    table
};

/// For each lane `x + 5y`, the lane that π moves there (Algorithm 3).
pub(crate) const PI_SOURCE: [usize; LANES] = {
    let mut table = [0; LANES];
    let mut rest: &mut [usize] = &mut table;
    let mut lane = 0;
    while let [slot, tail @ ..] = rest {
        *slot = pi_source(lane % 5, lane / 5);
        rest = tail;
        lane = lane.wrapping_add(1);
    }
    table
};

/// rc(t), Algorithm 5: the output bit of an 8-bit linear feedback shift register after
/// t mod 255 steps. Bit i of `r` holds R[i].
const fn rc(t: u32) -> u64 {
    let steps = t % 255;
    let mut r: u32 = 1; // R = 10000000
    let mut i = 0;
    while i < steps {
        r <<= 1; // R = 0 || R
        let r8 = (r >> 8) & 1;
        r ^= r8 | (r8 << 4) | (r8 << 5) | (r8 << 6); // R[0], R[4], R[5], R[6] ⊕= R[8]
        r &= 0xFF; // R = Trunc8[R]
        i = i.wrapping_add(1);
    }
    (r & 1) as u64
}

/// The round constant RC of round `ir`, Algorithm 6 steps 2–3: RC[2^j − 1] = rc(j + 7·ir)
/// for j = 0..=ℓ, and every other bit is 0.
const fn round_constant(ir: u32) -> u64 {
    let mut lane = 0;
    let mut j = 0;
    while j <= L {
        let bit = (1u32 << j).wrapping_sub(1);
        lane |= rc(j.wrapping_add(ir.wrapping_mul(7))).wrapping_shl(bit);
        j = j.wrapping_add(1);
    }
    lane
}

/// The ρ rotation of lane (x, y), Algorithm 2. Lane (0, 0) is not rotated (step 1). From
/// (x, y) = (1, 0), step t of the walk (x, y) ← (y, (2x + 3y) mod 5) rotates the lane it is
/// on by (t + 1)(t + 2)/2 bits, modulo w. A rotation of z bits towards higher z is
/// `rotate_left(z)`.
const fn rho_offset(x: usize, y: usize) -> u32 {
    let (mut wx, mut wy) = (1, 0);
    let mut t: u32 = 0;
    while t < 24 {
        if wx == x && wy == y {
            return (t.wrapping_add(1).wrapping_mul(t.wrapping_add(2)) / 2) % 64;
        }
        (wx, wy) = (wy, wx.wrapping_mul(2).wrapping_add(wy.wrapping_mul(3)) % 5);
        t = t.wrapping_add(1);
    }
    0
}

/// The lane π moves to (x, y), Algorithm 3: A′[x, y] = A[(x + 3y) mod 5, x].
const fn pi_source(x: usize, y: usize) -> usize {
    (x.wrapping_add(y.wrapping_mul(3)) % 5).wrapping_add(x.wrapping_mul(5))
}

/// Keccak-f[1600] (Algorithm 7 with n_r = 24, §3.4): rounds i_r = 0..=23 of
/// Rnd(A, i_r) = ι(χ(π(ρ(θ(A)))), i_r).
pub(crate) fn keccak_f1600(a: &mut [u64; LANES]) {
    // By reference: iterating the array by value would copy all 24 constants on every call.
    for &rc in &ROUND_CONSTANTS {
        round(a, rc);
    }
}

/// One round. Rows of the state are the 5-lane chunks `x + 5y` for fixed y.
fn round(a: &mut [u64; LANES], rc: u64) {
    // θ, Algorithm 1: C[x] is the parity of column x, and D[x] = C[x − 1] ⊕ rot(C[x + 1], 1)
    // is added to every lane of column x.
    let mut c = [0u64; 5];
    for row in a.as_chunks::<5>().0 {
        for (c, lane) in c.iter_mut().zip(row) {
            *c ^= lane;
        }
    }
    let [c0, c1, c2, c3, c4] = c;
    let d = [
        c4 ^ c1.rotate_left(1),
        c0 ^ c2.rotate_left(1),
        c1 ^ c3.rotate_left(1),
        c2 ^ c4.rotate_left(1),
        c3 ^ c0.rotate_left(1),
    ];
    for row in a.as_chunks_mut::<5>().0 {
        for (lane, d) in row.iter_mut().zip(d) {
            *lane ^= d;
        }
    }
    // ρ, Algorithm 2.
    for (lane, offset) in a.iter_mut().zip(RHO) {
        *lane = lane.rotate_left(offset);
    }
    // π, Algorithm 3. The sources are compile-time constants; after unrolling, every access
    // is at a fixed offset.
    let mut b = [0u64; LANES];
    for (lane, source) in b.iter_mut().zip(PI_SOURCE) {
        *lane = a.get(source).copied().unwrap_or(0);
    }
    // χ, Algorithm 4: A[x, y] = B[x, y] ⊕ (¬B[x + 1, y] ∧ B[x + 2, y]), row by row.
    let (rows, _) = a.as_chunks_mut::<5>();
    for (out, row) in rows.iter_mut().zip(b.as_chunks::<5>().0) {
        let [b0, b1, b2, b3, b4] = *row;
        *out = [
            b0 ^ (!b1 & b2),
            b1 ^ (!b2 & b3),
            b2 ^ (!b3 & b4),
            b3 ^ (!b4 & b0),
            b4 ^ (!b0 & b1),
        ];
    }
    // ι, Algorithm 6 step 4: RC is added to lane (0, 0).
    a[0] ^= rc;
}
