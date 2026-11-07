//! PS/2 keyboard scancode decoding (CLAUDE.md §56: keyboards). Scan code set 1 (the set
//! an i8042 controller delivers by default after translation) is turned into key events:
//! a physical key, whether it was pressed or released, and — through a US QWERTY layout
//! with the modifier state — the character it types, if any.
//!
//! Pure and table-driven, tested on the host; the kernel's i8042 driver (`src/i8042.rs`)
//! feeds bytes in. The scancode tables and the US layout are authored here from the
//! published set-1 code assignments, not copied.
//!
//! Set 1: a make code is one byte (0x01–0x58 for the main keys), a break code is the make
//! code with bit 7 set (`+ 0x80`). `0xE0` introduces an extended key (arrows, right-hand
//! modifiers, and so on); its next byte, masked, selects the key.

/// A physical key, named by what it is on a US keyboard. Only the keys set 1's one-byte
/// and `E0` forms reach are listed; anything else decodes to [`Key::Unknown`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Key {
    // Letters.
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    // Digits (the number row).
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    // The symbol keys of the main block.
    Grave,
    Minus,
    Equals,
    LeftBracket,
    RightBracket,
    Backslash,
    Semicolon,
    Apostrophe,
    Comma,
    Period,
    Slash,
    // Whitespace and editing.
    Space,
    Enter,
    Tab,
    Backspace,
    Escape,
    // Modifiers.
    LeftShift,
    RightShift,
    LeftCtrl,
    RightCtrl,
    LeftAlt,
    RightAlt,
    CapsLock,
    // Navigation (mostly E0-extended).
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// A function key F1–F12 (the number is 1–12).
    Function(u8),
    /// A key this decoder does not name.
    Unknown,
}

/// A decoded key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub key: Key,
    /// True on a make code (key down), false on a break code (key up).
    pub pressed: bool,
}

/// Which modifier keys are held, and whether Caps Lock is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub caps_lock: bool,
}

impl Modifiers {
    /// Updates the state from an event; returns whether this event was a modifier (so the
    /// caller knows it typed nothing).
    pub fn update(&mut self, event: Event) -> bool {
        match event.key {
            Key::LeftShift | Key::RightShift => self.shift = event.pressed,
            Key::LeftCtrl | Key::RightCtrl => self.ctrl = event.pressed,
            Key::LeftAlt | Key::RightAlt => self.alt = event.pressed,
            Key::CapsLock => {
                if event.pressed {
                    self.caps_lock = !self.caps_lock;
                }
            }
            _ => return false,
        }
        true
    }
}

/// The one-byte set-1 make code (0x00–0x58) to [`Key`] table.
const fn base_key(code: u8) -> Key {
    use Key::*;
    match code {
        0x01 => Escape,
        0x02 => Num1,
        0x03 => Num2,
        0x04 => Num3,
        0x05 => Num4,
        0x06 => Num5,
        0x07 => Num6,
        0x08 => Num7,
        0x09 => Num8,
        0x0A => Num9,
        0x0B => Num0,
        0x0C => Minus,
        0x0D => Equals,
        0x0E => Backspace,
        0x0F => Tab,
        0x10 => Q,
        0x11 => W,
        0x12 => E,
        0x13 => R,
        0x14 => T,
        0x15 => Y,
        0x16 => U,
        0x17 => I,
        0x18 => O,
        0x19 => P,
        0x1A => LeftBracket,
        0x1B => RightBracket,
        0x1C => Enter,
        0x1D => LeftCtrl,
        0x1E => A,
        0x1F => S,
        0x20 => D,
        0x21 => F,
        0x22 => G,
        0x23 => H,
        0x24 => J,
        0x25 => K,
        0x26 => L,
        0x27 => Semicolon,
        0x28 => Apostrophe,
        0x29 => Grave,
        0x2A => LeftShift,
        0x2B => Backslash,
        0x2C => Z,
        0x2D => X,
        0x2E => C,
        0x2F => V,
        0x30 => B,
        0x31 => N,
        0x32 => M,
        0x33 => Comma,
        0x34 => Period,
        0x35 => Slash,
        0x36 => RightShift,
        0x38 => LeftAlt,
        0x39 => Space,
        0x3A => CapsLock,
        0x3B..=0x44 => Function(code - 0x3A), // F1..F10
        0x57 => Function(11),
        0x58 => Function(12),
        _ => Unknown,
    }
}

/// The `E0`-extended make code (low 7 bits) to [`Key`] table.
const fn extended_key(code: u8) -> Key {
    use Key::*;
    match code {
        0x1D => RightCtrl,
        0x38 => RightAlt,
        0x1C => Enter, // keypad Enter
        0x35 => Slash, // keypad /
        0x47 => Home,
        0x48 => Up,
        0x49 => PageUp,
        0x4B => Left,
        0x4D => Right,
        0x4F => End,
        0x50 => Down,
        0x51 => PageDown,
        0x52 => Insert,
        0x53 => Delete,
        _ => Unknown,
    }
}

/// The incremental decoder: it holds the one-byte `E0` prefix state between bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Decoder {
    /// Set when the previous byte was `0xE0`.
    extended: bool,
}

impl Decoder {
    pub const fn new() -> Self {
        Self { extended: false }
    }

    /// Feeds one scancode byte; returns a key event once one is complete (the `0xE0`
    /// prefix byte itself, and the unused `0xE1` pause sequence, return `None`).
    pub fn feed(&mut self, byte: u8) -> Option<Event> {
        if byte == 0xE0 {
            self.extended = true;
            return None;
        }
        if byte == 0xE1 {
            // The Pause/Break key's multi-byte sequence is not decoded.
            self.extended = false;
            return None;
        }
        let extended = core::mem::take(&mut self.extended);
        let pressed = byte & 0x80 == 0;
        let code = byte & 0x7F;
        let key = if extended {
            extended_key(code)
        } else {
            base_key(code)
        };
        Some(Event { key, pressed })
    }
}

/// The character a key types under `mods` on the US QWERTY layout, or `None` if it types
/// nothing (a modifier, a navigation key, a release, Ctrl held, and so on).
pub fn to_char(event: Event, mods: Modifiers) -> Option<char> {
    if !event.pressed || mods.ctrl || mods.alt {
        return None;
    }
    use Key::*;
    // Letters: Caps Lock xor Shift gives upper case.
    let upper = mods.shift ^ mods.caps_lock;
    let letter = |lower: char, up: char| Some(if upper { up } else { lower });
    // Symbols and digits: Shift gives the upper symbol; Caps Lock does not affect them.
    let shifted = |base: char, shift: char| Some(if mods.shift { shift } else { base });
    match event.key {
        A => letter('a', 'A'),
        B => letter('b', 'B'),
        C => letter('c', 'C'),
        D => letter('d', 'D'),
        E => letter('e', 'E'),
        F => letter('f', 'F'),
        G => letter('g', 'G'),
        H => letter('h', 'H'),
        I => letter('i', 'I'),
        J => letter('j', 'J'),
        K => letter('k', 'K'),
        L => letter('l', 'L'),
        M => letter('m', 'M'),
        N => letter('n', 'N'),
        O => letter('o', 'O'),
        P => letter('p', 'P'),
        Q => letter('q', 'Q'),
        R => letter('r', 'R'),
        S => letter('s', 'S'),
        T => letter('t', 'T'),
        U => letter('u', 'U'),
        V => letter('v', 'V'),
        W => letter('w', 'W'),
        X => letter('x', 'X'),
        Y => letter('y', 'Y'),
        Z => letter('z', 'Z'),
        Num1 => shifted('1', '!'),
        Num2 => shifted('2', '@'),
        Num3 => shifted('3', '#'),
        Num4 => shifted('4', '$'),
        Num5 => shifted('5', '%'),
        Num6 => shifted('6', '^'),
        Num7 => shifted('7', '&'),
        Num8 => shifted('8', '*'),
        Num9 => shifted('9', '('),
        Num0 => shifted('0', ')'),
        Grave => shifted('`', '~'),
        Minus => shifted('-', '_'),
        Equals => shifted('=', '+'),
        LeftBracket => shifted('[', '{'),
        RightBracket => shifted(']', '}'),
        Backslash => shifted('\\', '|'),
        Semicolon => shifted(';', ':'),
        Apostrophe => shifted('\'', '"'),
        Comma => shifted(',', '<'),
        Period => shifted('.', '>'),
        Slash => shifted('/', '?'),
        Space => Some(' '),
        Enter => Some('\n'),
        Tab => Some('\t'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::string::String;

    /// Types a byte stream through a decoder and layout, collecting the characters.
    fn typed(bytes: &[u8]) -> String {
        let mut dec = Decoder::new();
        let mut mods = Modifiers::default();
        let mut out = String::new();
        for &b in bytes {
            if let Some(ev) = dec.feed(b) {
                if mods.update(ev) {
                    continue;
                }
                if let Some(c) = to_char(ev, mods) {
                    out.push(c);
                }
            }
        }
        out
    }

    #[test]
    fn plain_lowercase_and_a_release() {
        // h e l l o  (make codes), each followed by its break code.
        let bytes = [0x23, 0xA3, 0x12, 0x92, 0x26, 0xA6, 0x26, 0xA6, 0x18, 0x98];
        assert_eq!(typed(&bytes), "hello");
    }

    #[test]
    fn shift_makes_capitals_and_symbols() {
        // LeftShift down, h, i, LeftShift up  -> "HI"; then "1" and shifted "1" -> "1!".
        let bytes = [0x2A, 0x23, 0x17, 0xAA, 0x02, 0x2A, 0x02, 0xAA];
        assert_eq!(typed(&bytes), "HI1!");
    }

    #[test]
    fn caps_lock_affects_letters_not_digits() {
        // CapsLock, a, 1, (shift)a -> "A1a": caps makes 'a' upper, leaves '1', and
        // shift+caps on a letter is lower again.
        let bytes = [0x3A, 0x1E, 0x02, 0x2A, 0x1E, 0xAA];
        assert_eq!(typed(&bytes), "A1a");
    }

    #[test]
    fn ctrl_and_modifiers_type_nothing() {
        let mut dec = Decoder::new();
        let mut mods = Modifiers::default();
        // Ctrl down (E0 1D), c, c up, Ctrl up.
        for b in [0xE0, 0x1D] {
            if let Some(ev) = dec.feed(b) {
                assert!(mods.update(ev));
            }
        }
        assert!(mods.ctrl);
        let c = dec.feed(0x2E).unwrap();
        assert_eq!(
            c,
            Event {
                key: Key::C,
                pressed: true
            }
        );
        assert_eq!(to_char(c, mods), None, "Ctrl+C types nothing");
    }

    #[test]
    fn extended_keys_decode() {
        let mut dec = Decoder::new();
        assert_eq!(dec.feed(0xE0), None, "the prefix alone");
        assert_eq!(
            dec.feed(0x48),
            Some(Event {
                key: Key::Up,
                pressed: true
            })
        );
        assert_eq!(dec.feed(0xE0), None);
        assert_eq!(
            dec.feed(0xC8),
            Some(Event {
                key: Key::Up,
                pressed: false
            }),
            "release"
        );
        assert_eq!(
            dec.feed(0xE0).or(dec.feed(0x4D)),
            Some(Event {
                key: Key::Right,
                pressed: true
            })
        );
        // A right-hand Ctrl is distinct from the left.
        assert_eq!(dec.feed(0xE0), None);
        assert_eq!(
            dec.feed(0x1D),
            Some(Event {
                key: Key::RightCtrl,
                pressed: true
            })
        );
    }

    #[test]
    fn make_and_break_and_function_keys() {
        let mut dec = Decoder::new();
        assert_eq!(
            dec.feed(0x1E),
            Some(Event {
                key: Key::A,
                pressed: true
            })
        );
        assert_eq!(
            dec.feed(0x9E),
            Some(Event {
                key: Key::A,
                pressed: false
            })
        );
        assert_eq!(
            dec.feed(0x3B),
            Some(Event {
                key: Key::Function(1),
                pressed: true
            })
        );
        assert_eq!(
            dec.feed(0x44),
            Some(Event {
                key: Key::Function(10),
                pressed: true
            })
        );
        assert_eq!(
            dec.feed(0x58),
            Some(Event {
                key: Key::Function(12),
                pressed: true
            })
        );
        assert_eq!(
            dec.feed(0x00),
            Some(Event {
                key: Key::Unknown,
                pressed: true
            })
        );
    }

    #[test]
    fn a_full_sentence_round_trips() {
        // Type "Origin OS!" : Shift+o, r, i, g, i, n, space, Shift+o, Shift+s, Shift+1.
        let mut bytes = std::vec::Vec::new();
        let press = |b: &mut std::vec::Vec<u8>, code: u8| {
            b.push(code);
            b.push(code | 0x80);
        };
        let shifted = |b: &mut std::vec::Vec<u8>, code: u8| {
            b.push(0x2A);
            b.push(code);
            b.push(code | 0x80);
            b.push(0xAA);
        };
        shifted(&mut bytes, 0x18); // O
        press(&mut bytes, 0x13); // r
        press(&mut bytes, 0x17); // i
        press(&mut bytes, 0x22); // g
        press(&mut bytes, 0x17); // i
        press(&mut bytes, 0x31); // n
        press(&mut bytes, 0x39); // space
        shifted(&mut bytes, 0x18); // O
        shifted(&mut bytes, 0x1F); // S
        shifted(&mut bytes, 0x02); // !
        assert_eq!(typed(&bytes), "Origin OS!");
    }
}
