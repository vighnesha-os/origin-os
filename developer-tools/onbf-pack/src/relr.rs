//! RELR base-relative relocations: the `SHT_RELR` encoding of the ELF gABI, which ONBF §6.1
//! uses for the RELOCS section.
//!
//! The section is a sequence of little-endian `u64` words:
//! - a word with bit 0 clear is the RVA of a slot to relocate; the next location is
//!   that RVA + 8;
//! - a word with bit 0 set is a bitmap: for `i` in `0..63`, bit `i + 1` set means "relocate
//!   `location + 8·i`"; afterwards `location += 63·8`.
//!
//! The loader applies `*(u64 *)(base + rva) += base` at each slot (wrapping), so every slot
//! holds its addend in place (an implicit addend).
//!
//! [`encode`] writes the greedy encoding `rust-lld` also uses: one address word, then
//! bitmap words for as long as each next 63-slot window holds a slot; otherwise a new
//! address word. It is a function of the slot set alone, so equal relocations always give
//! equal bytes (ONBF-R9). It is also the shortest: call the start of the next window the
//! frontier (every slot below it is covered). After any number of words, the greedy
//! frontier is at least that of any other valid encoding with as many words, because a
//! bitmap at a lower frontier or an address word at the first uncovered slot never moves
//! past where the greedy step moves. (An informal exchange argument, not a machine proof.)

use core::fmt;

/// Slots covered by one bitmap word.
pub const BITMAP_SLOTS: u64 = 63;
/// Size of one slot and of one word.
pub const WORD: u64 = 8;
/// Bytes one bitmap word advances the location by.
const WINDOW: u64 = BITMAP_SLOTS * WORD;

/// Why a RELR word sequence or slot list is not acceptable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelrError {
    /// The byte length is not a multiple of 8.
    Length(usize),
    /// A bitmap word comes before any address word.
    BitmapWithoutAddress {
        /// Index of the word.
        word: usize,
    },
    /// An address or bitmap position overflows 64 bits.
    Overflow {
        /// Index of the word.
        word: usize,
    },
    /// A slot is not above the previous one: the slots must be strictly ascending, which
    /// also rules out a slot relocated twice.
    NotAscending {
        /// The offending slot.
        slot: u64,
    },
    /// A slot to encode is not 8-byte aligned.
    Misaligned {
        /// The offending slot.
        slot: u64,
    },
}

impl fmt::Display for RelrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Length(n) => write!(f, "{n} bytes is not a whole number of 8-byte words"),
            Self::BitmapWithoutAddress { word } => {
                write!(f, "word {word} is a bitmap with no address word before it")
            }
            Self::Overflow { word } => write!(f, "word {word} overflows the address space"),
            Self::NotAscending { slot } => {
                write!(f, "slot {slot:#x} is not above the previous slot")
            }
            Self::Misaligned { slot } => write!(f, "slot {slot:#x} is not 8-byte aligned"),
        }
    }
}

impl std::error::Error for RelrError {}

/// Appends `slot`, which must be above the last slot.
fn push(out: &mut Vec<u64>, slot: u64) -> Result<(), RelrError> {
    if out.last().is_some_and(|&last| slot <= last) {
        return Err(RelrError::NotAscending { slot });
    }
    out.push(slot);
    Ok(())
}

/// Decodes RELR words into the slots they relocate, which must be strictly ascending.
///
/// Alignment is not checked here; callers check each slot against the image.
pub fn decode(words: &[u64]) -> Result<Vec<u64>, RelrError> {
    let mut out = Vec::new();
    // The location the next bitmap word starts at, once an address word has been seen.
    let mut location: Option<u64> = None;
    for (word, &w) in words.iter().enumerate() {
        let overflow = RelrError::Overflow { word };
        if w & 1 == 0 {
            push(&mut out, w)?;
            location = Some(w.checked_add(WORD).ok_or(overflow)?);
            continue;
        }
        let base = location.ok_or(RelrError::BitmapWithoutAddress { word })?;
        let mut bits = w >> 1;
        let mut offset = 0u64;
        while bits != 0 {
            if bits & 1 == 1 {
                push(&mut out, base.checked_add(offset).ok_or(overflow)?)?;
            }
            bits >>= 1;
            offset = offset.checked_add(WORD).ok_or(overflow)?;
        }
        location = Some(base.checked_add(WINDOW).ok_or(overflow)?);
    }
    Ok(out)
}

/// Encodes strictly ascending, 8-byte-aligned slots as RELR words (see the module docs).
pub fn encode(slots: &[u64]) -> Result<Vec<u64>, RelrError> {
    let mut previous: Option<u64> = None;
    for &slot in slots {
        if slot & 7 != 0 {
            return Err(RelrError::Misaligned { slot });
        }
        if previous.is_some_and(|p| slot <= p) {
            return Err(RelrError::NotAscending { slot });
        }
        previous = Some(slot);
    }

    let mut words = Vec::new();
    let mut rest = slots;
    while let Some((&address, tail)) = rest.split_first() {
        let word = words.len();
        words.push(address);
        rest = tail;
        let mut base = address
            .checked_add(WORD)
            .ok_or(RelrError::Overflow { word })?;
        loop {
            let mut bitmap = 0u64;
            while let Some((&slot, tail)) = rest.split_first() {
                // `slot >= base`: the slots ascend, and every slot below `base` is covered.
                let index = slot.checked_sub(base).map_or(u64::MAX, |d| d / WORD);
                if index >= BITMAP_SLOTS {
                    break;
                }
                bitmap |= 1u64 << index;
                rest = tail;
            }
            if bitmap == 0 {
                break;
            }
            words.push((bitmap << 1) | 1);
            base = base
                .checked_add(WINDOW)
                .ok_or(RelrError::Overflow { word })?;
        }
    }
    Ok(words)
}

/// The words as little-endian bytes, the RELOCS section content.
pub fn words_to_bytes(words: &[u64]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Little-endian bytes as words.
pub fn words_from_bytes(bytes: &[u8]) -> Result<Vec<u64>, RelrError> {
    let (words, rest) = bytes.as_chunks::<8>();
    if !rest.is_empty() {
        return Err(RelrError::Length(bytes.len()));
    }
    Ok(words.iter().map(|w| u64::from_le_bytes(*w)).collect())
}
