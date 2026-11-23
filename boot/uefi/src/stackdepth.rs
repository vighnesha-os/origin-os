//! TASKS item 127: how deep the loader's stack goes before `ExitBootServices`, measured by paint.
//!
//! This module exists only under `cfg(origin_stack_measure)`, and in the host tests: no image a
//! normal or bench build makes holds it (`cargo xtask check`, TASKS item 112's F1 and F2, which
//! name this flag beside the bench flag). The measurement build is never signed and never rented.
//!
//! The method (the coordinating session's rulings on TASKS item 127, Q1 to Q4):
//! - At entry, the loader paints every 8-byte word of the stack UEFI promises it, from
//!   [`PROMISED`] below the entry RSP up to [`MARGIN`] below the painter's own RSP, and nothing
//!   beyond it (UEFI 2.11 §2.3.4: 128 KiB or more during boot services).
//! - At the end of each [`Phase`], the lowest word that no longer holds [`PAINT`] is the deepest
//!   point written since the last paint: by Origin's code, by the firmware's services it called,
//!   or by the firmware's timer interrupt. Its distance from the entry RSP is the phase's depth.
//!   The line is written, and then the range below the current RSP is painted again.
//! - Before the first phase, a calibration writes a known array on the stack, and its scan must
//!   find that array's lowest word, or a word at most [`CALIBRATION_SLACK`] below it
//!   ([`Calibration`]): the positive control.
//!
//! It reads and writes the stack through functions and holds no memory of its own, so it needs no
//! `unsafe`; `main.rs` gives it the volatile accesses.
#![forbid(unsafe_code)]

use core::fmt;

/// The stack UEFI promises an image while boot services run: 128 KiB or more (UEFI 2.11 §2.3.4).
pub const PROMISED: u64 = 128 * 1024;
/// Left unpainted just below the RSP a paint starts from: the painter's frame, and what an
/// interrupt taken while it paints may push.
pub const MARGIN: u64 = 512;
/// The size of a word the paint writes.
pub const WORD: u64 = 8;
/// The calibration's array: half of what UEFI promises.
pub const CALIBRATION_BYTES: usize = 64 * 1024;
/// How far below the calibration array's lowest word its scan may find the deepest change: the
/// calibrating function's own spills and anything an interrupt pushed while it ran (the
/// contract's SD-4).
pub const CALIBRATION_SLACK: u64 = 512;

/// The pattern written over every painted word. It is neither a canonical address (its bits 63 to
/// 47 are not all equal) nor a small integer, so a word the loader or the firmware writes is
/// unlikely to hold it by chance; a word that does is not counted, as in TASKS item 68.
pub const PAINT: u64 = 0xC3A7_59E1_0B6D_F28B;

/// Every line this module writes starts with this; F2 requires it in every measurement image and
/// refuses it in every other image.
pub const MARKER: &str = "stack measure";

/// Where the paint lies: `[bottom, top - PROMISED)` is never touched, `[bottom, end)` is painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Painted {
    /// The RSP at entry, from which every depth is counted.
    pub top: u64,
    /// The lowest painted word: `top - PROMISED`, rounded up to a word.
    pub bottom: u64,
    /// One past the highest painted word.
    pub end: u64,
}

impl Painted {
    /// The range for an entry RSP `top`, painted up to [`MARGIN`] below `rsp`, the painter's own
    /// RSP. `None` if `top` is too low to hold the promise, or `rsp` is above `top` or leaves
    /// nothing to paint.
    pub fn new(top: u64, rsp: u64) -> Option<Self> {
        let bottom = top.checked_sub(PROMISED)?.checked_next_multiple_of(WORD)?;
        let end = rsp.checked_sub(MARGIN)? & !(WORD - 1);
        (rsp <= top && end > bottom).then_some(Self { top, bottom, end })
    }

    /// The paint as it stands after [`Painted::new`] for `top` gave `end`: `None` while nothing is
    /// painted (either is 0) or if `end` is not above the promise's bottom.
    pub fn with_end(top: u64, end: u64) -> Option<Self> {
        let bottom = top.checked_sub(PROMISED)?.checked_next_multiple_of(WORD)?;
        (top != 0 && end > bottom && end <= top).then_some(Self { top, bottom, end })
    }

    /// The number of painted words.
    pub fn words(self) -> u64 {
        (self.end - self.bottom) / WORD
    }

    /// The address of painted word `i`, the lowest being 0.
    pub fn word_address(self, i: u64) -> u64 {
        self.bottom + i * WORD
    }
}

/// How deep a phase went, from what its scan found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// No painted word changed: the phase stayed above the paint's end.
    Above(u64),
    /// The lowest changed word was this many bytes below the entry RSP.
    Bytes(u64),
    /// The lowest painted word itself changed: the phase went to the bottom of the promise, or
    /// past it, which the paint cannot see.
    AtLeast(u64),
}

impl Depth {
    /// The deepest figure this says, in bytes below the entry RSP.
    pub const fn bytes(self) -> u64 {
        match self {
            Self::Above(b) | Self::Bytes(b) | Self::AtLeast(b) => b,
        }
    }

    /// One word for a static: the kind in bits 63 and 62, the bytes below. Depths are at most
    /// [`PROMISED`], far below bit 62.
    pub const fn encode(self) -> u64 {
        match self {
            Self::Above(b) => b & BYTES_MASK,
            Self::Bytes(b) => (1 << 62) | (b & BYTES_MASK),
            Self::AtLeast(b) => (2 << 62) | (b & BYTES_MASK),
        }
    }

    /// The depth [`Depth::encode`] gave `word` for.
    pub const fn decode(word: u64) -> Self {
        let b = word & BYTES_MASK;
        match word >> 62 {
            0 => Self::Above(b),
            1 => Self::Bytes(b),
            _ => Self::AtLeast(b),
        }
    }
}

/// The bytes of an encoded [`Depth`].
const BYTES_MASK: u64 = (1 << 62) - 1;

/// Scans `painted` from its lowest word up and stops at the first that no longer holds [`PAINT`].
/// `word(address)` reads the word there. A painted word above a changed one (a hole a frame
/// reserved and never wrote) does not shorten the depth.
pub fn scan(painted: Painted, mut word: impl FnMut(u64) -> u64) -> Depth {
    match (0..painted.words()).find(|&i| word(painted.word_address(i)) != PAINT) {
        None => Depth::Above(painted.top - painted.end),
        Some(0) => Depth::AtLeast(painted.top - painted.bottom),
        Some(i) => Depth::Bytes(painted.top - painted.word_address(i)),
    }
}

/// The parts of the loader's run before the call, in order (the rank-2 list's steps,
/// `docs/research/2026-11-21-loader-before-the-call.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The banner, the boot services table, the kernel file and any bench switch files.
    Read,
    /// `kernel::verify`: SHA-512, Ed25519, ML-DSA-87 and each section's Merkle check.
    Verify,
    /// The kernel placed, the generator started, the first slide.
    Place,
    /// The allocations, the memory map's size, the framebuffer.
    Allocate,
    /// The RSDP, the page tables, and the walks to the SPCR (and in a bench build the FADT, the
    /// DSDT and the FCH).
    Walks,
    /// The last lines, the bench loader's reads before the call, the last memory map, the boot
    /// info and the mark, up to the call itself.
    Call,
}

impl Phase {
    /// All six, in order.
    pub const ALL: [Self; 6] = [
        Self::Read,
        Self::Verify,
        Self::Place,
        Self::Allocate,
        Self::Walks,
        Self::Call,
    ];

    /// The phase numbered `n` by [`Phase::number`], if any.
    pub fn from_number(n: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.number() == n)
    }

    /// The phase's number, 1 to 6.
    pub const fn number(self) -> u8 {
        match self {
            Self::Read => 1,
            Self::Verify => 2,
            Self::Place => 3,
            Self::Allocate => 4,
            Self::Walks => 5,
            Self::Call => 6,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Read => "reading the kernel",
            Self::Verify => "verifying it",
            Self::Place => "placing it and starting the generator",
            Self::Allocate => "the allocations and the framebuffer",
            Self::Walks => "the page tables and the table walks",
            Self::Call => "the last lines, the last map and the mark, to the call",
        }
    }
}

/// One phase's line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseLine {
    /// The phase.
    pub phase: Phase,
    /// What its scan found.
    pub depth: Depth,
}

impl fmt::Display for PhaseLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "origin-boot: {MARKER}: P{} {}: ",
            self.phase.number(),
            self.phase.name()
        )?;
        write_depth(f, self.depth)
    }
}

fn write_depth(f: &mut fmt::Formatter<'_>, depth: Depth) -> fmt::Result {
    match depth {
        Depth::Above(b) => write!(f, "no deeper than {b} bytes (the paint's end)"),
        Depth::Bytes(b) => write!(f, "{b} bytes of the {PROMISED} UEFI promises"),
        Depth::AtLeast(b) => write!(
            f,
            "{b} bytes or more: the bottom of the {PROMISED} UEFI promises was written"
        ),
    }
}

/// The calibration: where the known array's lowest word lay, and what the scan found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    /// The array's lowest word's distance below the entry RSP.
    pub expected: u64,
    /// What the scan found.
    pub found: Depth,
}

impl Calibration {
    /// Whether the scan found the array's lowest word, or a word at most [`CALIBRATION_SLACK`]
    /// below it.
    pub fn holds(self) -> bool {
        matches!(self.found, Depth::Bytes(d) if d >= self.expected && d - self.expected <= CALIBRATION_SLACK)
    }
}

impl fmt::Display for Calibration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "origin-boot: {MARKER}: calibration: a {CALIBRATION_BYTES}-byte array whose lowest word is {} bytes below the entry RSP; the scan found ",
            self.expected
        )?;
        write_depth(f, self.found)?;
        if self.holds() {
            write!(
                f,
                ": {} bytes below the array, within the {CALIBRATION_SLACK} allowed",
                self.found.bytes() - self.expected
            )
        } else {
            f.write_str(": WRONG")
        }
    }
}

/// The last line: the deepest of the six, or why there was no measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Summary {
    /// The deepest phase and its depth.
    Deepest(Phase, Depth),
    /// The entry RSP could not hold the promise, so nothing was painted.
    NotPainted,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "origin-boot: {MARKER}: ")?;
        match *self {
            Self::Deepest(phase, depth) => {
                write!(f, "deepest before the call: P{}, ", phase.number())?;
                write_depth(f, depth)
            }
            Self::NotPainted => {
                f.write_str("not painted: the entry RSP is below the 128 KiB UEFI promises")
            }
        }
    }
}

/// The phases' depths as they come, for the summary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Record {
    deepest: Option<(Phase, Depth)>,
}

impl Record {
    /// The record whose deepest phase so far is `deepest`.
    pub const fn of(deepest: Option<(Phase, Depth)>) -> Self {
        Self { deepest }
    }

    /// The deepest phase so far, and its depth.
    pub const fn deepest(self) -> Option<(Phase, Depth)> {
        self.deepest
    }

    /// This record with `phase`'s depth noted: it becomes the deepest only if it is deeper than
    /// the deepest so far, so the first of two equal depths stays.
    #[must_use]
    pub fn note(self, phase: Phase, depth: Depth) -> Self {
        if self.deepest.is_none_or(|(_, d)| depth.bytes() > d.bytes()) {
            Self::of(Some((phase, depth)))
        } else {
            self
        }
    }

    /// The summary line, once the last phase is noted.
    pub fn summary(self) -> Summary {
        self.deepest
            .map_or(Summary::NotPainted, |(p, d)| Summary::Deepest(p, d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A fake stack: every word of `painted` holds [`PAINT`], but the words in `written`.
    fn stack(painted: Painted, written: &[u64]) -> HashMap<u64, u64> {
        let mut m: HashMap<u64, u64> = (0..painted.words())
            .map(|i| (painted.word_address(i), PAINT))
            .collect();
        for &a in written {
            m.insert(a, 0);
        }
        m
    }

    const TOP: u64 = 0x7F00_0000;

    #[test]
    fn the_range_is_the_promise_and_leaves_the_margin() {
        let p = Painted::new(TOP, TOP - 0x40).expect("a range");
        assert_eq!(p.bottom, TOP - PROMISED);
        assert_eq!(p.end, TOP - 0x40 - MARGIN);
        assert_eq!(p.words(), (PROMISED - 0x40 - MARGIN) / WORD);
        // Never below the promise, whatever the RSP.
        let deep = Painted::new(TOP, TOP - 0x1000).expect("a range");
        assert_eq!(deep.bottom, TOP - PROMISED);
    }

    #[test]
    fn an_unaligned_top_paints_only_whole_words_inside_the_promise() {
        let p = Painted::new(TOP + 4, TOP - 0x43).expect("a range");
        assert_eq!(p.bottom % WORD, 0);
        assert!(p.bottom >= TOP + 4 - PROMISED);
        assert_eq!(p.end % WORD, 0);
        assert!(p.end <= TOP - 0x43 - MARGIN);
    }

    #[test]
    fn no_range_where_the_promise_cannot_be_held() {
        assert_eq!(Painted::new(PROMISED - 8, PROMISED - 16), None);
        assert_eq!(Painted::new(TOP, TOP + 8), None, "an RSP above the top");
        assert_eq!(
            Painted::new(TOP, TOP - PROMISED + MARGIN),
            None,
            "nothing left to paint"
        );
    }

    #[test]
    fn an_untouched_paint_is_above_its_end() {
        let p = Painted::new(TOP, TOP - 0x100).expect("a range");
        let s = stack(p, &[]);
        assert_eq!(scan(p, |a| s[&a]), Depth::Above(0x100 + MARGIN));
    }

    #[test]
    fn the_lowest_change_is_the_depth_and_a_hole_above_it_does_not_count() {
        let p = Painted::new(TOP, TOP - 0x100).expect("a range");
        // A frame reserved 40 KiB, wrote its lowest word and its top, and left the rest painted.
        let low = TOP - 40 * 1024;
        let s = stack(p, &[low, p.end - WORD]);
        assert_eq!(scan(p, |a| s[&a]), Depth::Bytes(40 * 1024));
    }

    #[test]
    fn the_bottom_word_changed_is_saturation() {
        let p = Painted::new(TOP, TOP - 0x100).expect("a range");
        let s = stack(p, &[p.bottom]);
        assert_eq!(scan(p, |a| s[&a]), Depth::AtLeast(PROMISED));
        assert!(
            PhaseLine {
                phase: Phase::Verify,
                depth: Depth::AtLeast(PROMISED)
            }
            .to_string()
            .contains("or more")
        );
    }

    #[test]
    fn a_word_that_holds_the_paint_by_chance_is_not_counted() {
        let p = Painted::new(TOP, TOP - 0x100).expect("a range");
        let s = stack(p, &[TOP - 0x2000]);
        // Below it, a frame wrote the paint's own value: indistinguishable, and not counted.
        let mut t = s.clone();
        t.insert(TOP - 0x3000, PAINT);
        assert_eq!(scan(p, |a| t[&a]), Depth::Bytes(0x2000));
    }

    #[test]
    fn the_paint_is_no_canonical_address_and_no_small_integer() {
        let high = PAINT >> 47;
        assert!(high != 0 && high != (1 << 17) - 1);
        assert!(PAINT > u64::from(u32::MAX));
        assert!(PAINT.wrapping_neg() > u64::from(u32::MAX));
    }

    #[test]
    fn the_calibration_holds_only_at_or_just_below_its_array() {
        for (found, below) in [
            (0x1_0040, 0),
            (0x1_0048, 8),
            (0x1_0040 + CALIBRATION_SLACK, CALIBRATION_SLACK),
        ] {
            let c = Calibration {
                expected: 0x1_0040,
                found: Depth::Bytes(found),
            };
            assert!(c.holds(), "{found:#x}");
            assert!(
                c.to_string().ends_with(&format!(
                    ": {below} bytes below the array, within the 512 allowed"
                )),
                "{c}"
            );
        }
        for found in [
            Depth::Bytes(0x1_0040 + CALIBRATION_SLACK + 8),
            Depth::Bytes(0x1_0038),
            Depth::Above(0x200),
            Depth::AtLeast(PROMISED),
        ] {
            let c = Calibration {
                expected: 0x1_0040,
                found,
            };
            assert!(!c.holds(), "{found:?}");
            assert!(c.to_string().ends_with(": WRONG"));
        }
    }

    #[test]
    fn the_summary_names_the_deepest_phase() {
        let r = Record::default();
        assert_eq!(r.summary(), Summary::NotPainted);
        let r = r
            .note(Phase::Read, Depth::Bytes(3000))
            .note(Phase::Verify, Depth::Bytes(30_000))
            .note(Phase::Place, Depth::Above(900))
            .note(Phase::Call, Depth::Bytes(30_000));
        assert_eq!(
            r.summary(),
            Summary::Deepest(Phase::Verify, Depth::Bytes(30_000)),
            "the first of two equal depths stays"
        );
        assert_eq!(
            r.summary().to_string(),
            "origin-boot: stack measure: deepest before the call: P2, 30000 bytes of the 131072 UEFI promises"
        );
    }

    #[test]
    fn a_depth_survives_its_encoding_and_a_phase_its_number() {
        for d in [
            Depth::Above(0),
            Depth::Above(MARGIN),
            Depth::Bytes(8),
            Depth::Bytes(PROMISED - 8),
            Depth::AtLeast(PROMISED),
        ] {
            assert_eq!(Depth::decode(d.encode()), d);
        }
        for p in Phase::ALL {
            assert_eq!(Phase::from_number(p.number()), Some(p));
        }
        assert_eq!(Phase::from_number(0), None);
        assert_eq!(Phase::from_number(7), None);
        let r = Record::of(Some((Phase::Walks, Depth::Bytes(4096))));
        assert_eq!(r.deepest(), Some((Phase::Walks, Depth::Bytes(4096))));
    }

    #[test]
    fn the_paint_is_found_again_from_its_end() {
        let p = Painted::new(TOP, TOP - 0x100).expect("a range");
        assert_eq!(Painted::with_end(TOP, p.end), Some(p));
        assert_eq!(Painted::with_end(TOP, 0), None, "nothing painted");
        assert_eq!(Painted::with_end(0, p.end), None, "no top");
        assert_eq!(Painted::with_end(TOP, p.bottom), None, "an empty paint");
        assert_eq!(
            Painted::with_end(TOP, TOP + 8),
            None,
            "an end above the top"
        );
    }

    #[test]
    fn every_line_carries_the_marker() {
        for phase in Phase::ALL {
            let line = PhaseLine {
                phase,
                depth: Depth::Bytes(8),
            }
            .to_string();
            assert!(line.starts_with("origin-boot: stack measure: P"), "{line}");
        }
        assert!(Summary::NotPainted.to_string().contains(MARKER));
    }
}
