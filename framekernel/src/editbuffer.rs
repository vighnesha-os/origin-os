//! A gap buffer for editable text (CLAUDE.md §51 text editing, selection and caret
//! movement). A gap buffer keeps the text in one array with a movable gap at the caret, so
//! typing and deleting at the caret are amortised O(1) and only moving the caret costs work
//! — the usual structure behind a text field or a line editor. It stores Unicode scalars
//! (`char`), so a caret position is a scalar index; grapheme-aware movement is a display
//! layer added later over [`crate::text`]. Pure, no `unsafe`.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

/// How many scalar slots the gap grows by when it fills.
const GROW: usize = 32;

/// An editable text buffer with a caret, backed by a gap buffer. The logical text is the
/// scalars outside the gap: `slots[..gap_start]` then `slots[gap_end..]`; the caret sits at
/// `gap_start` (logical position `gap_start`).
pub struct EditBuffer {
    slots: Vec<char>,
    gap_start: usize,
    gap_end: usize,
}

impl EditBuffer {
    /// An empty buffer with the caret at 0.
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            gap_start: 0,
            gap_end: 0,
        }
    }

    /// The number of scalars in the text (excluding the gap).
    pub fn len(&self) -> usize {
        self.slots.len() - (self.gap_end - self.gap_start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The caret's logical position (0..=len).
    pub fn caret(&self) -> usize {
        self.gap_start
    }

    /// Ensures the gap holds at least one slot, growing it in place if empty.
    fn ensure_gap(&mut self) {
        if self.gap_start == self.gap_end {
            // Insert GROW placeholder slots at the gap; they are never read as text because
            // they lie in [gap_start, gap_end).
            let at = self.gap_end;
            self.slots.splice(at..at, core::iter::repeat_n('\0', GROW));
            self.gap_end += GROW;
        }
    }

    /// Inserts `c` at the caret and advances the caret past it.
    pub fn insert(&mut self, c: char) {
        self.ensure_gap();
        self.slots[self.gap_start] = c;
        self.gap_start += 1;
    }

    /// Inserts every scalar of `s` at the caret.
    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.insert(c);
        }
    }

    /// Deletes the scalar before the caret (backspace); returns whether one was removed.
    pub fn delete_back(&mut self) -> bool {
        if self.gap_start == 0 {
            return false;
        }
        self.gap_start -= 1; // the scalar before the caret falls into the gap
        true
    }

    /// Deletes the scalar at the caret (forward delete); returns whether one was removed.
    pub fn delete_forward(&mut self) -> bool {
        if self.gap_end == self.slots.len() {
            return false;
        }
        self.gap_end += 1; // the scalar after the caret falls into the gap
        true
    }

    /// Moves the caret one scalar left; returns whether it moved.
    pub fn move_left(&mut self) -> bool {
        if self.gap_start == 0 {
            return false;
        }
        self.gap_start -= 1;
        self.gap_end -= 1;
        self.slots[self.gap_end] = self.slots[self.gap_start];
        true
    }

    /// Moves the caret one scalar right; returns whether it moved.
    pub fn move_right(&mut self) -> bool {
        if self.gap_end == self.slots.len() {
            return false;
        }
        self.slots[self.gap_start] = self.slots[self.gap_end];
        self.gap_start += 1;
        self.gap_end += 1;
        true
    }

    /// Moves the caret to logical position `pos` (clamped to `len`).
    pub fn move_to(&mut self, pos: usize) {
        let pos = pos.min(self.len());
        while self.gap_start > pos {
            self.move_left();
        }
        while self.gap_start < pos {
            self.move_right();
        }
    }

    /// The scalar at logical position `i`, or `None` if out of range.
    pub fn get(&self, i: usize) -> Option<char> {
        if i < self.gap_start {
            self.slots.get(i).copied()
        } else if i < self.len() {
            self.slots.get(i + (self.gap_end - self.gap_start)).copied()
        } else {
            None
        }
    }

    /// The whole text collected into a `String` (an alias for `ToString::to_string`, which
    /// the [`core::fmt::Display`] impl provides).
    pub fn text(&self) -> String {
        let mut s = String::with_capacity(self.len());
        s.extend(&self.slots[..self.gap_start]);
        s.extend(&self.slots[self.gap_end..]);
        s
    }
}

impl Default for EditBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Display for EditBuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for &c in &self.slots[..self.gap_start] {
            write!(f, "{c}")?;
        }
        for &c in &self.slots[self.gap_end..] {
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

impl core::fmt::Debug for EditBuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EditBuffer")
            .field("len", &self.len())
            .field("caret", &self.caret())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_editing_and_caret() {
        let mut b = EditBuffer::new();
        assert!(b.is_empty());
        b.insert_str("helo");
        assert_eq!(b.to_string(), "helo");
        assert_eq!(b.caret(), 4);
        // Move back and fix the typo: "helo" -> "hello".
        b.move_left(); // caret before 'o'
        b.insert('l');
        assert_eq!(b.to_string(), "hello");
        assert_eq!(b.caret(), 4);
        // Backspace and forward-delete.
        b.move_to(0);
        assert!(!b.delete_back(), "nothing before the start");
        assert!(b.delete_forward());
        assert_eq!(b.to_string(), "ello");
        b.move_to(b.len());
        assert!(b.delete_back());
        assert_eq!(b.to_string(), "ell");
        assert!(!b.delete_forward(), "nothing after the end");
    }

    #[test]
    fn handles_non_ascii_scalars() {
        let mut b = EditBuffer::new();
        b.insert_str("café ∀😀");
        assert_eq!(b.len(), 7, "seven scalars regardless of byte length");
        b.move_to(4); // after "café"
        b.insert_str(" au lait");
        assert_eq!(b.to_string(), "café au lait ∀😀");
        assert_eq!(b.get(3), Some('é'));
        assert_eq!(b.get(b.len()), None);
    }

    #[test]
    fn caret_clamps_and_moves_across_the_gap() {
        let mut b = EditBuffer::new();
        b.insert_str("abcdef");
        b.move_to(1000);
        assert_eq!(b.caret(), 6, "clamped to len");
        b.move_to(3);
        assert_eq!(b.caret(), 3);
        assert_eq!(b.get(3), Some('d'));
        // Inserting in the middle keeps both halves intact.
        b.insert('X');
        assert_eq!(b.to_string(), "abcXdef");
    }

    /// Model-based test: thousands of random edits must keep the buffer's text and caret
    /// identical to a straightforward `Vec<char>` + cursor model.
    #[test]
    fn matches_a_vec_model_under_random_edits() {
        let mut b = EditBuffer::new();
        let mut text: Vec<char> = Vec::new();
        let mut caret = 0usize;
        let letters = ['a', 'z', 'é', '∀', '😀', '\n'];
        let mut x = 0x1234_5678_9abc_def1u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };

        for _ in 0..50_000 {
            match next() % 7 {
                0 | 1 => {
                    let c = letters[(next() as usize) % letters.len()];
                    b.insert(c);
                    text.insert(caret, c);
                    caret += 1;
                }
                2 => {
                    if b.delete_back() {
                        assert!(caret > 0);
                        text.remove(caret - 1);
                        caret -= 1;
                    } else {
                        assert_eq!(caret, 0);
                    }
                }
                3 => {
                    if b.delete_forward() {
                        assert!(caret < text.len());
                        text.remove(caret);
                    } else {
                        assert_eq!(caret, text.len());
                    }
                }
                4 => {
                    if b.move_left() {
                        caret -= 1;
                    } else {
                        assert_eq!(caret, 0);
                    }
                }
                5 => {
                    if b.move_right() {
                        caret += 1;
                    } else {
                        assert_eq!(caret, text.len());
                    }
                }
                _ => {
                    let pos = (next() as usize) % (text.len() + 1);
                    b.move_to(pos);
                    caret = pos;
                }
            }
            // The invariants must hold after every single operation.
            assert_eq!(b.caret(), caret);
            assert_eq!(b.len(), text.len());
        }
        let model: String = text.iter().collect();
        assert_eq!(b.to_string(), model);
        // Every position reads back the model's scalar.
        for (i, &c) in text.iter().enumerate() {
            assert_eq!(b.get(i), Some(c));
        }
    }
}
