//! HMAC_DRBG over SHA-512: the deterministic random bit generator of NIST SP 800-90A Rev. 1
//! §10.1.2, with HMAC-SHA-512 (FIPS 198-1) as its one primitive.
//!
//! This is the mechanism only. It makes no entropy: every seed is the caller's to supply, and a
//! caller that has none gets an error, never a default (ADR-0016). What a seed must hold, where
//! it comes from, how the sources are tested and when a generator is reseeded are decided
//! there, for the Framekernel and the boot loader, which are this module's users.
//!
//! The standard's state is a key, a value `V` and a count of requests since the last seed.
//! [`HmacDrbgSha512`] holds the key as HMAC's two starting states (see `crate::hmac`), and
//! overwrites everything it computes on the way, as far as `crate::wipe` can promise. Nothing
//! here branches on or indexes by a secret: the branches are on lengths and on the count.
//!
//! In the standard's words, with `K` the key and `provided_data` possibly empty:
//!
//! ```text
//! Update(provided_data):  K = HMAC(K, V || 0x00 || provided_data);  V = HMAC(K, V)
//!                         if provided_data is not empty:
//!                         K = HMAC(K, V || 0x01 || provided_data);  V = HMAC(K, V)
//! Instantiate:            K = 0x00 ... 00;  V = 0x01 ... 01
//!                         Update(entropy_input || nonce || personalization_string);  count = 1
//! Reseed:                 Update(entropy_input || additional_input);  count = 1
//! Generate:               if count > reseed_interval: "reseed required"
//!                         if additional_input is not empty: Update(additional_input)
//!                         repeat V = HMAC(K, V), output V, until enough; keep the leftmost bytes
//!                         Update(additional_input);  count = count + 1
//! ```

use core::fmt;

use crate::Sha512;
use crate::block::copy;
use crate::hmac::Key;
use crate::wipe::wipe;

/// HMAC-SHA-512's output length, which is the length of `V` and of the standard's key.
const OUT: usize = Sha512::OUTPUT_LEN;

/// Why a request was refused. A refused request changes nothing: not the generator's state and
/// not the caller's buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrbgError {
    /// The generator holds no state: it was never instantiated, or it was uninstantiated.
    NotInstantiated,
    /// [`HmacDrbgSha512::instantiate`] on a generator that holds a state. Uninstantiate it
    /// first: a state is never replaced by accident.
    AlreadyInstantiated,
    /// The entropy input is shorter than [`HmacDrbgSha512::MIN_ENTROPY_LEN`].
    EntropyTooShort,
    /// The nonce is shorter than [`HmacDrbgSha512::MIN_NONCE_LEN`].
    NonceTooShort,
    /// An entropy input, nonce, personalization string or additional input is longer than
    /// [`HmacDrbgSha512::MAX_INPUT_LEN`].
    InputTooLong,
    /// More than [`HmacDrbgSha512::MAX_REQUEST_LEN`] bytes were asked for in one request.
    RequestTooLong,
    /// The generator has answered as many requests as its reseed interval allows. It must be
    /// reseeded before it answers another (the standard's "reseed required").
    ReseedRequired,
}

/// One HMAC_DRBG instantiation at SHA-512's highest security strength, 256 bits.
///
/// A new value holds no state. [`Self::instantiate`] gives it one, [`Self::generate`] and
/// [`Self::reseed`] work on it, and [`Self::uninstantiate`], or dropping the value, overwrites
/// it. The value is neither `Clone` nor `Copy`: two copies of one state would give the same
/// output twice.
pub struct HmacDrbgSha512 {
    key: Key,
    v: [u8; OUT],
    /// The standard's `reseed_counter`: one more than the requests answered since the last seed.
    reseed_counter: u64,
    reseed_interval: u64,
    instantiated: bool,
}

/// Prints nothing of the state (CLAUDE.md §61).
impl fmt::Debug for HmacDrbgSha512 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HmacDrbgSha512 { .. }")
    }
}

impl Default for HmacDrbgSha512 {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HmacDrbgSha512 {
    fn drop(&mut self) {
        self.uninstantiate();
    }
}

impl HmacDrbgSha512 {
    /// The security strength of every instantiation, in bits: the highest SP 800-90A allows
    /// for SHA-512.
    pub const SECURITY_STRENGTH_BITS: u32 = 256;
    /// The shortest entropy input, in bytes: as many bits as the security strength. The caller
    /// answers for those bytes holding that much entropy; this type can only count them.
    pub const MIN_ENTROPY_LEN: usize = 32;
    /// The shortest nonce at instantiation, in bytes: half the security strength.
    pub const MIN_NONCE_LEN: usize = 16;
    /// The longest entropy input, nonce, personalization string or additional input, in bytes:
    /// the standard's 2^35 bits.
    pub const MAX_INPUT_LEN: u64 = 1 << 32;
    /// The most bytes one request may ask for: the standard's 2^19 bits.
    pub const MAX_REQUEST_LEN: usize = 1 << 16;
    /// The longest reseed interval, in requests: the standard's 2^48.
    pub const MAX_RESEED_INTERVAL: u64 = 1 << 48;

    /// A generator with no state, and the longest reseed interval the standard allows.
    pub const fn new() -> Self {
        Self {
            key: Key::unset(),
            v: [0; OUT],
            reseed_counter: 0,
            reseed_interval: Self::MAX_RESEED_INTERVAL,
            instantiated: false,
        }
    }

    /// A generator with no state, which answers `reseed_interval` requests between seeds and
    /// then asks to be reseeded. `None` for an interval of zero or one above
    /// [`Self::MAX_RESEED_INTERVAL`].
    pub const fn with_reseed_interval(reseed_interval: u64) -> Option<Self> {
        if reseed_interval == 0 || reseed_interval > Self::MAX_RESEED_INTERVAL {
            return None;
        }
        Some(Self {
            key: Key::unset(),
            v: [0; OUT],
            reseed_counter: 0,
            reseed_interval,
            instantiated: false,
        })
    }

    /// Whether the generator holds a state.
    pub const fn is_instantiated(&self) -> bool {
        self.instantiated
    }

    /// The requests answered since the generator was last seeded, or 0 with no state.
    pub const fn requests_since_seed(&self) -> u64 {
        self.reseed_counter.saturating_sub(1)
    }

    /// Whether the next request would be refused with [`DrbgError::ReseedRequired`].
    pub const fn reseed_required(&self) -> bool {
        self.instantiated && self.reseed_counter > self.reseed_interval
    }

    /// The standard's instantiate function (§10.1.2.3): gives the generator its first state
    /// from `entropy_input || nonce || personalization_string`.
    ///
    /// `entropy_input` must hold at least 256 bits of entropy in at least
    /// [`Self::MIN_ENTROPY_LEN`] bytes. `nonce` must be at least [`Self::MIN_NONCE_LEN`] bytes
    /// that hold 128 bits of entropy or do not repeat (§8.6.7). `personalization_string` may be
    /// empty; it is how one instantiation is told from another, and need not be secret.
    pub fn instantiate(
        &mut self,
        entropy_input: &[u8],
        nonce: &[u8],
        personalization_string: &[u8],
    ) -> Result<(), DrbgError> {
        if self.instantiated {
            return Err(DrbgError::AlreadyInstantiated);
        }
        check_entropy(entropy_input)?;
        if nonce.len() < Self::MIN_NONCE_LEN {
            return Err(DrbgError::NonceTooShort);
        }
        check_input(nonce)?;
        check_input(personalization_string)?;
        self.key.set(&[0x00; OUT]);
        self.v = [0x01; OUT];
        self.update([entropy_input, nonce, personalization_string]);
        self.reseed_counter = 1;
        self.instantiated = true;
        Ok(())
    }

    /// The standard's reseed function (§10.1.2.4): mixes `entropy_input || additional_input`
    /// into the state and starts the count of requests again.
    ///
    /// `entropy_input` is held to what [`Self::instantiate`] asks of its own. `additional_input`
    /// may be empty.
    pub fn reseed(
        &mut self,
        entropy_input: &[u8],
        additional_input: &[u8],
    ) -> Result<(), DrbgError> {
        if !self.instantiated {
            return Err(DrbgError::NotInstantiated);
        }
        check_entropy(entropy_input)?;
        check_input(additional_input)?;
        self.update([entropy_input, additional_input, &[]]);
        self.reseed_counter = 1;
        Ok(())
    }

    /// The standard's generate function (§10.1.2.5): fills `out`, then moves the state on so
    /// that the bytes handed out cannot be computed from it again.
    ///
    /// `additional_input` may be empty. Past the reseed interval the request is refused with
    /// [`DrbgError::ReseedRequired`] and `out` is not written.
    pub fn generate(&mut self, out: &mut [u8], additional_input: &[u8]) -> Result<(), DrbgError> {
        self.check_request(out, additional_input)?;
        if self.reseed_counter > self.reseed_interval {
            return Err(DrbgError::ReseedRequired);
        }
        if !additional_input.is_empty() {
            self.update([additional_input, &[], &[]]);
        }
        for chunk in out.chunks_mut(OUT) {
            self.next_v();
            copy(chunk, &self.v);
        }
        self.update([additional_input, &[], &[]]);
        self.reseed_counter = self.reseed_counter.saturating_add(1);
        Ok(())
    }

    /// A request with prediction resistance, and the answer to "reseed required" (§9.3.1,
    /// steps 7 and 8): reseeds from `entropy_input` together with `additional_input`, then
    /// generates with no additional input.
    ///
    /// The whole request is checked first, so one that would be refused consumes no entropy.
    pub fn reseed_and_generate(
        &mut self,
        out: &mut [u8],
        entropy_input: &[u8],
        additional_input: &[u8],
    ) -> Result<(), DrbgError> {
        self.check_request(out, additional_input)?;
        self.reseed(entropy_input, additional_input)?;
        self.generate(out, &[])
    }

    /// The standard's uninstantiate function (§9.4): overwrites the state. The generator can be
    /// instantiated again afterwards.
    pub fn uninstantiate(&mut self) {
        self.key.wipe();
        wipe(&mut self.v);
        self.reseed_counter = 0;
        self.instantiated = false;
    }

    /// Whether nothing of a state is held: as [`Self::new`] and [`Self::uninstantiate`] leave
    /// the generator.
    #[cfg(test)]
    pub(crate) fn state_is_overwritten(&self) -> bool {
        self.key.is_unset() && self.v == [0; OUT] && self.reseed_counter == 0 && !self.instantiated
    }

    /// What the generate function checks before it looks at the count (§9.3.1, steps 1 to 4).
    fn check_request(&self, out: &[u8], additional_input: &[u8]) -> Result<(), DrbgError> {
        if !self.instantiated {
            return Err(DrbgError::NotInstantiated);
        }
        if out.len() > Self::MAX_REQUEST_LEN {
            return Err(DrbgError::RequestTooLong);
        }
        check_input(additional_input)
    }

    /// `HMAC_DRBG_Update` (§10.1.2.2) with `provided` taken as one string, which is the
    /// standard's "Null" when every part is empty.
    fn update(&mut self, provided: [&[u8]; 3]) {
        self.next_key(0x00, provided);
        self.next_v();
        if provided.iter().any(|part| !part.is_empty()) {
            self.next_key(0x01, provided);
            self.next_v();
        }
    }

    /// `K = HMAC(K, V || separator || provided)`.
    fn next_key(&mut self, separator: u8, provided: [&[u8]; 3]) {
        let [first, second, third] = provided;
        let mut key = [0u8; OUT];
        self.key.mac(
            &[self.v.as_slice(), &[separator], first, second, third],
            &mut key,
        );
        self.key.set(&key);
        wipe(&mut key);
    }

    /// `V = HMAC(K, V)`.
    fn next_v(&mut self) {
        let mut v = [0u8; OUT];
        self.key.mac(&[self.v.as_slice()], &mut v);
        self.v = v;
        wipe(&mut v);
    }
}

/// An entropy input's length against the standard's `min_length` and `max_length`.
fn check_entropy(entropy_input: &[u8]) -> Result<(), DrbgError> {
    if entropy_input.len() < HmacDrbgSha512::MIN_ENTROPY_LEN {
        return Err(DrbgError::EntropyTooShort);
    }
    check_input(entropy_input)
}

/// Any other input's length against the standard's maximum.
fn check_input(input: &[u8]) -> Result<(), DrbgError> {
    match u64::try_from(input.len()) {
        Ok(len) if len <= HmacDrbgSha512::MAX_INPUT_LEN => Ok(()),
        _ => Err(DrbgError::InputTooLong),
    }
}
