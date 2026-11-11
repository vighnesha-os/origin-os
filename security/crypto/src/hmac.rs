//! HMAC-SHA-512: the keyed-hash message authentication code of FIPS 198-1 (and RFC 2104)
//! over SHA-512.
//!
//! `HMAC(K, text) = H((K0 ^ opad) || H((K0 ^ ipad) || text))`, where `K0` is the key made one
//! hash block long (hashed first if it is longer than a block, then filled with zeros), `ipad`
//! is the byte `0x36` repeated and `opad` the byte `0x5c` repeated (FIPS 198-1 §4, Table 1;
//! RFC 2104 §2).
//!
//! A key is secret, and from the first block on so is everything the two hashes hold. This
//! module therefore hashes through SHA-512's secret-material paths and overwrites every
//! buffer it fills, as far as `crate::wipe` can promise. Nothing here branches on or indexes
//! by a key or a message byte; lengths are treated as public.

use core::fmt;

use crate::Sha512;
use crate::block::copy;
use crate::wipe::wipe;

/// SHA-512's block length: the length of `K0`.
const BLOCK: usize = Sha512::BLOCK_LEN;
/// RFC 2104 §2: the byte 0x36, repeated to the block length.
const IPAD: u8 = 0x36;
/// RFC 2104 §2: the byte 0x5C, repeated to the block length.
const OPAD: u8 = 0x5c;

/// An HMAC key, held as the two hash states every MAC under it starts from: SHA-512 after
/// `K0 ^ ipad` and SHA-512 after `K0 ^ opad`. Holding these instead of the key makes a MAC of
/// a short message two compressions instead of four, and they are worth exactly what the key
/// is worth, so they are overwritten like one.
///
/// It is neither `Clone` nor `Copy`: a second copy of a key is a second thing to overwrite.
pub(crate) struct Key {
    inner: Sha512,
    outer: Sha512,
}

impl Key {
    /// No key yet. [`Self::set`] must run before [`Self::mac`] means anything.
    pub(crate) const fn unset() -> Self {
        Self {
            inner: Sha512::new(),
            outer: Sha512::new(),
        }
    }

    /// Replaces the key with `key`, in place.
    pub(crate) fn set(&mut self, key: &[u8]) {
        // K0 (FIPS 198-1 §4, steps 1 to 3): a key longer than a block is hashed first; the rest
        // of the block is zeros.
        let mut k0 = [0u8; BLOCK];
        if key.len() > BLOCK {
            let mut hash = Sha512::new();
            hash.update_secret(key);
            let mut digest = [0u8; Sha512::OUTPUT_LEN];
            hash.finalize_secret(&mut digest);
            copy(&mut k0, &digest);
            wipe(&mut digest);
        } else {
            copy(&mut k0, key);
        }
        for byte in &mut k0 {
            *byte ^= IPAD;
        }
        self.inner.wipe();
        self.inner.update_secret(&k0);
        // From `K0 ^ ipad` to `K0 ^ opad`.
        for byte in &mut k0 {
            *byte ^= IPAD ^ OPAD;
        }
        self.outer.wipe();
        self.outer.update_secret(&k0);
        wipe(&mut k0);
    }

    /// Writes to `out` the HMAC, under this key, of `parts` taken as one message: the
    /// concatenation is never built, so a caller needs no buffer for it.
    pub(crate) fn mac(&self, parts: &[&[u8]], out: &mut [u8; Sha512::OUTPUT_LEN]) {
        let mut hash = self.inner.clone();
        for part in parts {
            hash.update_secret(part);
        }
        self.finish(&mut hash, out);
    }

    /// Finishes a MAC whose message `inner` has absorbed, and overwrites `inner`.
    fn finish(&self, inner: &mut Sha512, out: &mut [u8; Sha512::OUTPUT_LEN]) {
        let mut digest = [0u8; Sha512::OUTPUT_LEN];
        inner.finalize_secret(&mut digest);
        let mut hash = self.outer.clone();
        hash.update_secret(&digest);
        hash.finalize_secret(out);
        wipe(&mut digest);
    }

    /// Overwrites the key (see `crate::wipe`).
    pub(crate) fn wipe(&mut self) {
        self.inner.wipe();
        self.outer.wipe();
    }

    /// Whether no key is held: as [`Self::unset`] and [`Self::wipe`] leave it.
    #[cfg(test)]
    pub(crate) fn is_unset(&self) -> bool {
        self.inner.is_fresh() && self.outer.is_fresh()
    }
}

/// Streaming HMAC-SHA-512.
///
/// The key and the message are overwritten when the value is finalized or dropped, as far as
/// this crate can promise without `unsafe` (the crate's documentation says how far that is).
/// Comparing a MAC with an expected one is the caller's to do, in constant time.
pub struct HmacSha512 {
    key: Key,
    /// The inner hash, which has absorbed `K0 ^ ipad` and the message so far.
    message: Sha512,
}

/// Prints nothing of the key or the message (CLAUDE.md §61).
impl fmt::Debug for HmacSha512 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HmacSha512 { .. }")
    }
}

impl Drop for HmacSha512 {
    fn drop(&mut self) {
        self.key.wipe();
        self.message.wipe();
    }
}

impl HmacSha512 {
    /// MAC length in bytes.
    pub const OUTPUT_LEN: usize = Sha512::OUTPUT_LEN;

    /// A MAC under `key`, of any length. A key shorter than [`Self::OUTPUT_LEN`] bytes is
    /// accepted, as FIPS 198-1 defines it, and is weaker than the hash (RFC 2104 §3).
    pub fn new(key: &[u8]) -> Self {
        let mut mac = Self {
            key: Key::unset(),
            message: Sha512::new(),
        };
        mac.key.set(key);
        mac.message = mac.key.inner.clone();
        mac
    }

    /// Absorbs `data` as the next part of the message.
    pub fn update(&mut self, data: &[u8]) {
        self.message.update_secret(data);
    }

    /// Returns the MAC of the message.
    pub fn finalize(mut self) -> [u8; Sha512::OUTPUT_LEN] {
        let mut out = [0u8; Sha512::OUTPUT_LEN];
        let Self { key, message } = &mut self;
        key.finish(message, &mut out);
        out
    }

    /// The HMAC-SHA-512 of `data` under `key`.
    pub fn mac(key: &[u8], data: &[u8]) -> [u8; Sha512::OUTPUT_LEN] {
        let mut keyed = Key::unset();
        keyed.set(key);
        let mut out = [0u8; Sha512::OUTPUT_LEN];
        keyed.mac(&[data], &mut out);
        keyed.wipe();
        out
    }
}
