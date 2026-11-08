//! ML-DSA-87 signing (FIPS 204, August 2024), for the build host only: key generation
//! (Algorithm 6), signing (Algorithms 2 and 7), and the encodings they need.
//!
//! The private key is the 32-byte seed ξ. From it [`SigningKey::from_seed`] derives ρ, K, tr
//! and s1, s2, t0, which are kept in memory only for as long as the key lives.
//!
//! Coefficients are kept reduced in [0, q). Everything computed from s1, s2, t0, K or the
//! masking vector y is constant time: reduction is Barrett with a mask-based conditional
//! subtraction, the NTT is a fixed sequence of operations, and the rejection checks combine
//! their results by mask before the one branch on the outcome (see the crate documentation).

use origin_crypto::{Shake128, Shake256};

use crate::wipe;
use crate::wipe_words;

pub(crate) const N: usize = 256;
pub(crate) const Q: u32 = 8380417;
/// Dropped bits of t (FIPS 204 Table 1).
const D: u32 = 13;
/// Challenge weight.
const TAU: usize = 60;
/// Range of the mask y.
const GAMMA1: u32 = 1 << 19;
/// Low-order rounding range.
pub(crate) const GAMMA2: u32 = (Q - 1) / 32;
/// Rows of A.
const K: usize = 8;
/// Columns of A.
const L: usize = 7;
/// Range of the secret coefficients.
const ETA: u32 = 2;
/// τ · η.
const BETA: u32 = 120;
/// Largest number of 1s in the hint.
const OMEGA: usize = 75;
/// c̃ is λ/4 bytes, λ = 256.
const CTILDE_BYTES: usize = 64;

/// Public-key length.
pub const PUBLIC_KEY_BYTES: usize = 2592;
/// Signature length.
pub const SIGNATURE_BYTES: usize = 4627;

pub(crate) type Poly = [u32; N];

// ------------------------------------------------------------------ the field

/// floor(2^78 / q), the Barrett constant for products below 2^46.
const BARRETT: u128 = (1u128 << 78) / Q as u128;

/// `r − q` if `r ≥ q`, for `r < q + 2^31`, by mask.
fn csub_q(r: u32) -> u32 {
    let d = r.wrapping_sub(Q);
    d.wrapping_add(Q & (((d as i32) >> 31) as u32))
}

/// `x mod q`, for `x < 2^46`. The Barrett estimate is at most one too small, so `x − t·q` is
/// below 2q and one conditional subtraction finishes.
pub(crate) fn reduce(x: u64) -> u32 {
    let t = ((u128::from(x) * BARRETT) >> 78) as u64;
    csub_q(x.wrapping_sub(t.wrapping_mul(u64::from(Q))) as u32)
}

fn add(a: u32, b: u32) -> u32 {
    csub_q(a + b)
}

fn sub(a: u32, b: u32) -> u32 {
    csub_q(a.wrapping_sub(b).wrapping_add(Q))
}

pub(crate) fn mul(a: u32, b: u32) -> u32 {
    reduce(u64::from(a) * u64::from(b))
}

/// x ∈ (−q, q) as a value in [0, q), by mask.
pub(crate) fn from_signed(x: i32) -> u32 {
    (x + (Q as i32 & (x >> 31))) as u32
}

/// The representative of x ∈ [0, q) in (−q/2, q/2], by mask.
pub(crate) fn centered(x: u32) -> i32 {
    let above = (((Q - 1) / 2) as i64 - i64::from(x)) >> 63; // all ones if x > (q − 1)/2
    x as i32 - (Q & above as u32) as i32
}

const fn pow_mod(base: u64, mut e: u32) -> u32 {
    let m = Q as u64;
    let mut b = base % m;
    let mut r = 1u64;
    while e > 0 {
        if e & 1 == 1 {
            r = r * b % m;
        }
        b = b * b % m;
        e >>= 1;
    }
    r as u32
}

const fn brv8(x: u32) -> u32 {
    let mut r = 0;
    let mut i = 0;
    while i < 8 {
        if x & (1 << i) != 0 {
            r |= 1 << (7 - i);
        }
        i += 1;
    }
    r
}

/// ζ^brv8(m), ζ = 1753 (FIPS 204 §7.5).
const ZETAS: [u32; N] = {
    let mut table = [0u32; N];
    let mut m = 0;
    while m < N {
        table[m] = pow_mod(1753, brv8(m as u32));
        m += 1;
    }
    table
};
/// 256⁻¹ mod q (Algorithm 42).
const N_INVERSE: u32 = 8347681;

/// NTT (Algorithm 41).
pub(crate) fn ntt(w: &mut Poly) {
    let mut m = 0;
    let mut len = N / 2;
    while len >= 1 {
        let mut start = 0;
        while start < N {
            m += 1;
            let z = ZETAS[m];
            for j in start..start + len {
                let t = mul(z, w[j + len]);
                w[j + len] = sub(w[j], t);
                w[j] = add(w[j], t);
            }
            start += 2 * len;
        }
        len /= 2;
    }
}

/// NTT⁻¹ (Algorithm 42).
pub(crate) fn inverse_ntt(w: &mut Poly) {
    let mut m = N;
    let mut len = 1;
    while len < N {
        let mut start = 0;
        while start < N {
            m -= 1;
            let z = Q - ZETAS[m];
            for j in start..start + len {
                let t = w[j];
                w[j] = add(t, w[j + len]);
                w[j + len] = mul(z, sub(t, w[j + len]));
            }
            start += 2 * len;
        }
        len *= 2;
    }
    for c in w.iter_mut() {
        *c = mul(N_INVERSE, *c);
    }
}

pub(crate) fn pointwise(a: &Poly, b: &Poly) -> Poly {
    core::array::from_fn(|i| mul(a[i], b[i]))
}

// ------------------------------------------------------------------ sampling

/// `H`: SHAKE256 of the concatenation of `parts`.
fn shake256(parts: &[&[u8]], out: &mut [u8]) {
    let mut h = Shake256::new();
    for part in parts {
        h.update(part);
    }
    h.finalize_xof().squeeze(out);
}

/// RejNTTPoly (Algorithm 30), with CoeffFromThreeBytes (Algorithm 14). ρ is public.
fn rej_ntt_poly(seed: &[u8; 34]) -> Poly {
    let mut h = Shake128::new();
    h.update(seed);
    let mut reader = h.finalize_xof();
    let mut poly = [0u32; N];
    let mut j = 0;
    while j < N {
        let mut bytes = [0u8; 3];
        reader.squeeze(&mut bytes);
        let z = u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2] & 0x7F) << 16;
        if z < Q {
            poly[j] = z;
            j += 1;
        }
    }
    poly
}

/// ExpandA (Algorithm 32): Â, row by row. Public.
fn expand_a(rho: &[u8; 32]) -> Box<[[Poly; L]; K]> {
    let mut a_hat = Box::new([[[0u32; N]; L]; K]);
    for (r, row) in a_hat.iter_mut().enumerate() {
        for (s, entry) in row.iter_mut().enumerate() {
            let mut seed = [0u8; 34];
            seed[..32].copy_from_slice(rho);
            seed[32] = s as u8;
            seed[33] = r as u8;
            *entry = rej_ntt_poly(&seed);
        }
    }
    a_hat
}

/// CoeffFromHalfByte (Algorithm 15) for η = 2: `2 − (b mod 5)` for `b < 15`. `None` rejects
/// the nibble. A rejected nibble reveals only that it was 15.
fn coeff_from_half_byte(b: u8) -> Option<u32> {
    if b >= 15 {
        return None;
    }
    // b mod 5 by multiply and shift, exact for b < 15.
    let quotient = (u32::from(b) * 205) >> 10;
    let remainder = u32::from(b) - 5 * quotient;
    Some(from_signed(ETA as i32 - remainder as i32))
}

/// RejBoundedPoly (Algorithm 31), into the caller's array.
fn rej_bounded_poly(seed: &[u8; 66], poly: &mut Poly) {
    let mut h = Shake256::new();
    h.update(seed);
    let mut reader = h.finalize_xof();
    let mut j = 0;
    while j < N {
        let mut byte = [0u8; 1];
        reader.squeeze(&mut byte);
        if let Some(c) = coeff_from_half_byte(byte[0] & 0xF) {
            poly[j] = c;
            j += 1;
        }
        if let (Some(c), true) = (coeff_from_half_byte(byte[0] >> 4), j < N) {
            poly[j] = c;
            j += 1;
        }
    }
}

/// ExpandS (Algorithm 33), into the caller's arrays: no copy of a secret polynomial is left
/// behind by a return.
fn expand_s(rho: &[u8; 64], s1: &mut [Poly; L], s2: &mut [Poly; K]) {
    let sample = |r: u16, poly: &mut Poly| {
        let mut seed = [0u8; 66];
        seed[..64].copy_from_slice(rho);
        seed[64..].copy_from_slice(&r.to_le_bytes());
        rej_bounded_poly(&seed, poly);
        wipe(&mut seed);
    };
    for (r, poly) in s1.iter_mut().enumerate() {
        sample(r as u16, poly);
    }
    for (r, poly) in s2.iter_mut().enumerate() {
        sample((L + r) as u16, poly);
    }
}

/// ExpandMask (Algorithm 34): y from ρ″ and the counter μ, each coefficient BitUnpack'ed
/// (Algorithm 19) with a = γ1 − 1 and b = γ1, so 20 bits each. Written into the caller's array.
pub(crate) fn expand_mask(rho: &[u8; 64], mu: u16, y: &mut [Poly; L]) {
    for (r, poly) in y.iter_mut().enumerate() {
        let mut v = [0u8; 640];
        shake256(&[rho, &mu.wrapping_add(r as u16).to_le_bytes()], &mut v);
        for (i, coefficient) in poly.iter_mut().enumerate() {
            let bit = 20 * i;
            let byte = bit / 8;
            let shift = bit % 8;
            let wide =
                u32::from(v[byte]) | u32::from(v[byte + 1]) << 8 | u32::from(v[byte + 2]) << 16;
            let z = (wide >> shift) & 0xF_FFFF;
            *coefficient = from_signed(GAMMA1 as i32 - z as i32);
        }
        wipe(&mut v);
    }
}

/// SampleInBall (Algorithm 29). c̃ is public in the finished signature.
pub(crate) fn sample_in_ball(ctilde: &[u8; CTILDE_BYTES]) -> Poly {
    let mut h = Shake256::new();
    h.update(ctilde);
    let mut reader = h.finalize_xof();
    let mut signs = [0u8; 8];
    reader.squeeze(&mut signs);
    let signs = u64::from_le_bytes(signs);
    let mut c = [0u32; N];
    for i in N - TAU..N {
        let j = loop {
            let mut byte = [0u8; 1];
            reader.squeeze(&mut byte);
            if usize::from(byte[0]) <= i {
                break usize::from(byte[0]);
            }
        };
        c[i] = c[j];
        let negative = (signs >> (i + TAU - N)) & 1 == 1;
        c[j] = if negative { Q - 1 } else { 1 };
    }
    c
}

// ------------------------------------------------------------------ rounding

/// Power2Round (Algorithm 35): `(r1, r0 mod q)`, for r ∈ [0, q).
pub(crate) fn power2round(r: u32) -> (u32, u32) {
    let r1 = (r + (1 << (D - 1)) - 1) >> D;
    let r0 = r as i32 - (r1 << D) as i32;
    (r1, from_signed(r0))
}

/// 2·γ2.
pub(crate) const ALPHA: u32 = 2 * GAMMA2;
/// floor(a / α) = (a · M) >> 46 for every a below q + γ2 (checked exhaustively in a test).
pub(crate) const DECOMPOSE_M: u64 = (1u64 << 46) / ALPHA as u64 + 1;

/// Decompose (Algorithm 36): `(r1, r0)` for r ∈ [0, q), by masks.
pub(crate) fn decompose(r: u32) -> (u32, i32) {
    // r mod± α leaves r1 = floor((r + γ2 − 1)/α), and r − r0 = α·r1.
    let r1 = (u64::from(r + GAMMA2 - 1) * DECOMPOSE_M) >> 46;
    let r1 = r1 as u32;
    let r0 = r as i32 - (r1 * ALPHA) as i32;
    // α·16 = q − 1, the wrap case of Algorithm 36.
    let wraps = equal_mask(r1, 16);
    (r1 & !wraps, r0 - (wraps as i32 & 1))
}

/// All ones if `a == b`, otherwise zero.
pub(crate) fn equal_mask(a: u32, b: u32) -> u32 {
    let x = a ^ b;
    (((x | x.wrapping_neg()) as i32) >> 31) as u32 ^ u32::MAX
}

/// MakeHint (Algorithm 39), as 0 or 1.
pub(crate) fn make_hint(z: u32, r: u32) -> u32 {
    let high = decompose(r).0;
    let shifted = decompose(add(r, z)).0;
    1 & !equal_mask(high, shifted)
}

/// All ones if any coefficient has |c| ≥ `bound`, otherwise zero, without branching.
pub(crate) fn any_at_least(poly: &Poly, bound: i32) -> u32 {
    let mut bad = 0;
    for &c in poly {
        let v = centered(c);
        let magnitude = (v ^ (v >> 31)) - (v >> 31);
        bad |= ((bound - 1 - magnitude) >> 31) as u32;
    }
    bad
}

// ------------------------------------------------------------------ encodings

/// Packs each coefficient's low `bits` bits, little-endian, into `out`, which must be exactly
/// `256 · bits / 8` bytes (SimpleBitPack, Algorithm 16).
fn pack_bits(coefficients: &Poly, bits: u32, out: &mut [u8]) {
    let mut accumulator = 0u64;
    let mut filled = 0u32;
    let mut at = 0;
    for &c in coefficients {
        accumulator |= u64::from(c) << filled;
        filled += bits;
        while filled >= 8 {
            out[at] = accumulator as u8;
            at += 1;
            accumulator >>= 8;
            filled -= 8;
        }
    }
}

/// The public-key encoding (Algorithm 22).
fn pk_encode(rho: &[u8; 32], t1: &[Poly; K]) -> Box<[u8; PUBLIC_KEY_BYTES]> {
    let mut pk = Box::new([0u8; PUBLIC_KEY_BYTES]);
    pk[..32].copy_from_slice(rho);
    for (poly, out) in t1.iter().zip(pk[32..].as_chunks_mut::<320>().0) {
        pack_bits(poly, 10, out);
    }
    pk
}

// ------------------------------------------------------------------ the key

/// Why a signing request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignError {
    /// The context string is longer than 255 bytes (Algorithm 2).
    ContextTooLong,
}

/// An ML-DSA-87 key pair. Every secret is in its own heap allocation, so moving the key copies
/// only pointers.
pub struct SigningKey {
    public_key: Box<[u8; PUBLIC_KEY_BYTES]>,
    k_secret: Box<[u8; 32]>,
    tr: [u8; 64],
    a_hat: Box<[[Poly; L]; K]>,
    s1_hat: Box<[Poly; L]>,
    s2_hat: Box<[Poly; K]>,
    t0_hat: Box<[Poly; K]>,
}

impl core::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SigningKey(..)")
    }
}

impl Drop for SigningKey {
    fn drop(&mut self) {
        wipe(self.k_secret.as_mut_slice());
        for poly in self.s1_hat.iter_mut() {
            wipe_words(poly);
        }
        for poly in self.s2_hat.iter_mut().chain(self.t0_hat.iter_mut()) {
            wipe_words(poly);
        }
    }
}

/// `Â ∘ v̂`, row by row, written into `out` in place: no intermediate polynomial is left on
/// the stack. `out` must hold zeros.
fn mat_vec(a_hat: &[[Poly; L]; K], v_hat: &[Poly; L], out: &mut [Poly; K]) {
    for (row, slot) in a_hat.iter().zip(out.iter_mut()) {
        for (entry, v) in row.iter().zip(v_hat) {
            for ((sum, &a), &b) in slot.iter_mut().zip(entry).zip(v) {
                *sum = add(*sum, mul(a, b));
            }
        }
    }
}

impl SigningKey {
    /// ML-DSA.KeyGen_internal (Algorithm 6) from the seed ξ.
    pub fn from_seed(xi: &[u8; 32]) -> Self {
        let mut seeds = [0u8; 128];
        shake256(&[xi, &[K as u8, L as u8]], &mut seeds);
        let mut rho = [0u8; 32];
        let mut rho_prime = [0u8; 64];
        let mut k_secret = Box::new([0u8; 32]);
        rho.copy_from_slice(&seeds[..32]);
        rho_prime.copy_from_slice(&seeds[32..96]);
        k_secret.copy_from_slice(&seeds[96..]);
        wipe(&mut seeds);

        let a_hat = expand_a(&rho);
        let mut s1_hat = Box::new([[0u32; N]; L]);
        let mut s2_hat = Box::new([[0u32; N]; K]);
        expand_s(&rho_prime, &mut s1_hat, &mut s2_hat);
        wipe(&mut rho_prime);

        for poly in s1_hat.iter_mut() {
            ntt(poly);
        }
        let mut t = [[0u32; N]; K];
        mat_vec(&a_hat, &s1_hat, &mut t);
        for (ti, s2i) in t.iter_mut().zip(s2_hat.iter()) {
            inverse_ntt(ti);
            for (c, &s) in ti.iter_mut().zip(s2i) {
                *c = add(*c, s);
            }
        }
        for poly in s2_hat.iter_mut() {
            ntt(poly);
        }

        let mut t1 = [[0u32; N]; K];
        let mut t0_hat = Box::new([[0u32; N]; K]);
        for ((ti, t1i), t0i) in t.iter().zip(t1.iter_mut()).zip(t0_hat.iter_mut()) {
            for ((c, r1), r0) in ti.iter().zip(t1i.iter_mut()).zip(t0i.iter_mut()) {
                let (high, low) = power2round(*c);
                *r1 = high;
                *r0 = low;
            }
            ntt(t0i);
        }
        for poly in t.iter_mut() {
            wipe_words(poly);
        }
        let public_key = pk_encode(&rho, &t1);
        let mut tr = [0u8; 64];
        shake256(&[public_key.as_slice()], &mut tr);

        Self {
            public_key,
            k_secret,
            tr,
            a_hat,
            s1_hat,
            s2_hat,
            t0_hat,
        }
    }

    /// The public key (Algorithm 22).
    pub fn public_key(&self) -> &[u8; PUBLIC_KEY_BYTES] {
        &self.public_key
    }

    /// ML-DSA.Sign (Algorithm 2) with `rnd` for the randomness (32 zero bytes is the
    /// deterministic variant, any other value hedged), through Sign_internal (Algorithm 7).
    pub fn sign(
        &self,
        message: &[u8],
        context: &[u8],
        rnd: &[u8; 32],
    ) -> Result<Box<[u8; SIGNATURE_BYTES]>, SignError> {
        let context_len = u8::try_from(context.len()).map_err(|_| SignError::ContextTooLong)?;

        // μ = H(tr ‖ M′, 64), M′ = 0 ‖ |ctx| ‖ ctx ‖ M (Algorithms 2 and 7).
        let mut mu = [0u8; 64];
        shake256(&[&self.tr, &[0, context_len], context, message], &mut mu);
        // ρ″ = H(K ‖ rnd ‖ μ, 64).
        let mut rho_pp = [0u8; 64];
        shake256(&[self.k_secret.as_slice(), rnd, &mu], &mut rho_pp);

        let mut kappa = 0u16;
        // The candidates live here, so each is wiped whether it is accepted or rejected.
        let mut y = [[0u32; N]; L];
        let mut y_hat = [[0u32; N]; L];
        let mut z = [[0u32; N]; L];
        let mut w = [[0u32; N]; K];
        let mut r = [[0u32; N]; K];
        let mut cs = [0u32; N];
        let mut w1_encoded = [0u8; 128 * K];
        let signature = loop {
            expand_mask(&rho_pp, kappa, &mut y);
            kappa = kappa.wrapping_add(L as u16);
            y_hat.copy_from_slice(&y);
            for poly in y_hat.iter_mut() {
                ntt(poly);
            }
            for poly in w.iter_mut() {
                poly.fill(0);
            }
            mat_vec(&self.a_hat, &y_hat, &mut w);
            for (wi, out) in w.iter_mut().zip(w1_encoded.as_chunks_mut::<128>().0) {
                inverse_ntt(wi);
                let high: Poly = core::array::from_fn(|i| decompose(wi[i]).0);
                pack_bits(&high, 4, out);
            }
            let mut ctilde = [0u8; CTILDE_BYTES];
            shake256(&[&mu, &w1_encoded], &mut ctilde);
            let mut c_hat = sample_in_ball(&ctilde);
            ntt(&mut c_hat);

            for ((zi, yi), s1i) in z.iter_mut().zip(&y).zip(self.s1_hat.iter()) {
                cs = pointwise(&c_hat, s1i);
                inverse_ntt(&mut cs);
                for ((out, &a), &b) in zi.iter_mut().zip(yi).zip(&cs) {
                    *out = add(a, b);
                }
            }
            // r = w − c·s2, and the first rejection check.
            let mut bad = 0u32;
            for ((ri, wi), s2i) in r.iter_mut().zip(&w).zip(self.s2_hat.iter()) {
                cs = pointwise(&c_hat, s2i);
                inverse_ntt(&mut cs);
                for ((out, &a), &b) in ri.iter_mut().zip(wi).zip(&cs) {
                    *out = sub(a, b);
                }
                let low: Poly = core::array::from_fn(|i| from_signed(decompose(ri[i]).1));
                bad |= any_at_least(&low, (GAMMA2 - BETA) as i32);
            }
            for zi in &z {
                bad |= any_at_least(zi, (GAMMA1 - BETA) as i32);
            }
            let mut accepted = None;
            if bad == 0 {
                // ct0 and the hint are computed only for a candidate that passed.
                let mut hints = [[0u32; N]; K];
                let mut count = 0u32;
                let mut bad_ct0 = 0u32;
                for ((t0i, ri), hi) in self.t0_hat.iter().zip(&r).zip(hints.iter_mut()) {
                    cs = pointwise(&c_hat, t0i);
                    inverse_ntt(&mut cs);
                    bad_ct0 |= any_at_least(&cs, GAMMA2 as i32);
                    for ((h, &r_c), &c) in hi.iter_mut().zip(ri).zip(&cs) {
                        *h = make_hint(sub(0, c), add(r_c, c));
                        count = count.wrapping_add(*h);
                    }
                }
                if bad_ct0 == 0 && count as usize <= OMEGA {
                    accepted = Some(encode_signature(&ctilde, &z, &hints));
                }
            }
            if let Some(signature) = accepted {
                break signature;
            }
        };
        wipe(&mut w1_encoded);
        wipe_words(&mut cs);
        for poly in y
            .iter_mut()
            .chain(y_hat.iter_mut())
            .chain(z.iter_mut())
            .chain(w.iter_mut())
            .chain(r.iter_mut())
        {
            wipe_words(poly);
        }
        wipe(&mut rho_pp);
        wipe(&mut mu);
        Ok(signature)
    }
}

/// sigEncode (Algorithm 26): c̃, then z with BitPack (a = γ1 − 1, b = γ1), then the hint with
/// HintBitPack (Algorithm 20).
fn encode_signature(
    ctilde: &[u8; CTILDE_BYTES],
    z: &[Poly; L],
    hints: &[Poly; K],
) -> Box<[u8; SIGNATURE_BYTES]> {
    let mut sig = Box::new([0u8; SIGNATURE_BYTES]);
    sig[..CTILDE_BYTES].copy_from_slice(ctilde);
    let (z_area, hint_area) = sig[CTILDE_BYTES..].split_at_mut(640 * L);
    for (zi, out) in z.iter().zip(z_area.as_chunks_mut::<640>().0) {
        let packed: Poly = core::array::from_fn(|i| (GAMMA1 as i32 - centered(zi[i])) as u32);
        pack_bits(&packed, 20, out);
    }
    let mut index = 0;
    let (positions, counts) = hint_area.split_at_mut(OMEGA);
    for (hi, count) in hints.iter().zip(counts.iter_mut()) {
        for (j, &h) in hi.iter().enumerate() {
            if h != 0 {
                positions[index] = j as u8;
                index += 1;
            }
        }
        *count = index as u8;
    }
    sig
}

#[cfg(test)]
pub(crate) mod testing {
    //! Internals for `src/tests.rs`.
    pub(crate) use super::{
        ALPHA, DECOMPOSE_M, GAMMA2, N, Poly, Q, any_at_least, centered, decompose, equal_mask,
        expand_mask, from_signed, inverse_ntt, make_hint, mul, ntt, pointwise, power2round, reduce,
        sample_in_ball,
    };
}
