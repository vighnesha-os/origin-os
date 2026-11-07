//! A JSON parser (RFC 8259) for configuration, package manifests and SDK data interchange
//! (CLAUDE.md §23 first-run config, §46 package metadata, §49 SDK). Recursive descent into
//! a [`Value`] tree, with a nesting-depth limit so a deeply nested (adversarial) document is
//! rejected rather than overflowing the stack (untrusted input, §34). Numbers are kept as
//! their validated source text — `core` has no float parser in `no_std`, and a systems
//! parser is better off deferring the interpretation to the caller; [`Value::as_i64`] reads
//! the common integer case. Pure, no `unsafe`; `alloc` for the tree.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

/// The maximum array/object nesting depth accepted.
pub const MAX_DEPTH: usize = 128;

/// A parsed JSON value. Objects keep their members in source order (no map dependency).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// A number in its validated source text (e.g. `"-12.5e3"`); see [`Value::as_i64`].
    Number(String),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    /// The value of an integer number, or `None` if it is not an integer that fits `i64`.
    pub fn as_i64(&self) -> Option<i64> {
        let Value::Number(n) = self else { return None };
        if n.contains(['.', 'e', 'E']) {
            return None; // not an integer form
        }
        n.parse::<i64>().ok()
    }

    /// The string, if this is a [`Value::Str`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The member named `key`, if this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

/// Parses a complete JSON document, or `None` if it is malformed, too deeply nested, or has
/// trailing content.
pub fn parse(input: &str) -> Option<Value> {
    let mut p = Parser {
        b: input.as_bytes(),
        i: 0,
        depth: 0,
    };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i == p.b.len() { Some(v) } else { None }
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    /// Consumes `expected` if it is next.
    fn eat(&mut self, expected: u8) -> Option<()> {
        (self.peek()? == expected).then(|| self.i += 1)
    }

    fn value(&mut self) -> Option<Value> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        let v = match self.peek()? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string().map(Value::Str),
            b't' => self.literal(b"true", Value::Bool(true)),
            b'f' => self.literal(b"false", Value::Bool(false)),
            b'n' => self.literal(b"null", Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        };
        self.depth -= 1;
        v
    }

    fn literal(&mut self, word: &[u8], value: Value) -> Option<Value> {
        if self.b.get(self.i..self.i + word.len())? == word {
            self.i += word.len();
            Some(value)
        } else {
            None
        }
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        // Integer part: a lone 0, or a non-zero digit then digits.
        match self.peek()? {
            b'0' => self.i += 1,
            b'1'..=b'9' => self.digits(),
            _ => return None,
        }
        // Fraction.
        if self.peek() == Some(b'.') {
            self.i += 1;
            if !self.peek()?.is_ascii_digit() {
                return None;
            }
            self.digits();
        }
        // Exponent.
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !self.peek()?.is_ascii_digit() {
                return None;
            }
            self.digits();
        }
        let text = core::str::from_utf8(&self.b[start..self.i]).ok()?;
        Some(Value::Number(String::from(text)))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut s = String::new();
        loop {
            match self.peek()? {
                b'"' => {
                    self.i += 1;
                    return Some(s);
                }
                b'\\' => {
                    self.i += 1;
                    match self.peek()? {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'b' => s.push('\u{0008}'),
                        b'f' => s.push('\u{000C}'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'u' => {
                            self.i += 1;
                            s.push(self.unicode_escape()?);
                            continue; // unicode_escape already advanced past the hex
                        }
                        _ => return None,
                    }
                    self.i += 1;
                }
                // A control character must be escaped (RFC 8259 §7); bytes < 0x20 are invalid.
                c if c < 0x20 => return None,
                _ => {
                    // Copy one UTF-8 scalar from the source.
                    let rest = self.b.get(self.i..)?;
                    let ch = core::str::from_utf8(rest).ok()?.chars().next()?;
                    s.push(ch);
                    self.i += ch.len_utf8();
                }
            }
        }
    }

    /// Reads the four hex digits of a `\u` escape (already past the `u`), combining a
    /// surrogate pair with a following `\u` escape.
    fn unicode_escape(&mut self) -> Option<char> {
        let hi = self.hex4()?;
        if (0xD800..=0xDBFF).contains(&hi) {
            // High surrogate: a low surrogate must follow as another \u escape.
            if self.b.get(self.i..self.i + 2)? != b"\\u" {
                return None;
            }
            self.i += 2;
            let lo = self.hex4()?;
            if !(0xDC00..=0xDFFF).contains(&lo) {
                return None;
            }
            let c = 0x1_0000 + ((u32::from(hi - 0xD800) << 10) | u32::from(lo - 0xDC00));
            char::from_u32(c)
        } else {
            char::from_u32(u32::from(hi))
        }
    }

    fn hex4(&mut self) -> Option<u16> {
        let mut v = 0u16;
        for _ in 0..4 {
            let d = (self.peek()? as char).to_digit(16)?;
            v = v << 4 | d as u16;
            self.i += 1;
        }
        Some(v)
    }

    fn array(&mut self) -> Option<Value> {
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Some(Value::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value()?);
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Some(Value::Array(items));
                }
                _ => return None,
            }
        }
    }

    fn object(&mut self) -> Option<Value> {
        self.eat(b'{')?;
        let mut members = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Some(Value::Object(members));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            let value = self.value()?;
            members.push((key, value));
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(Value::Object(members));
                }
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn parses_a_document() {
        let doc = r#"
          {
            "name": "origin-os",
            "version": 7,
            "stable": true,
            "ratio": -12.5e-2,
            "tags": ["kernel", "rust"],
            "nested": { "a": [1, 2, { "b": null }] },
            "unicode": "caf\u00e9 \uD83D\uDE00"
          }
        "#;
        let v = parse(doc).unwrap();
        assert_eq!(v.get("name").and_then(Value::as_str), Some("origin-os"));
        assert_eq!(v.get("version").and_then(Value::as_i64), Some(7));
        assert_eq!(v.get("stable"), Some(&Value::Bool(true)));
        assert_eq!(
            v.get("ratio").and_then(Value::as_i64),
            None,
            "a float is not an i64"
        );
        assert_eq!(
            v.get("ratio"),
            Some(&Value::Number("-12.5e-2".to_string())),
            "the number's source text is kept"
        );
        let tags = v.get("tags").unwrap();
        assert_eq!(
            tags,
            &Value::Array(alloc::vec![
                Value::Str("kernel".into()),
                Value::Str("rust".into())
            ])
        );
        // Nested access and the escaped Unicode (é and an emoji from a surrogate pair).
        assert_eq!(v.get("unicode").and_then(Value::as_str), Some("café 😀"));
        let nested = v.get("nested").unwrap().get("a").unwrap();
        assert_eq!(
            nested,
            &Value::Array(alloc::vec![
                Value::Number("1".into()),
                Value::Number("2".into()),
                Value::Object(alloc::vec![("b".into(), Value::Null)]),
            ])
        );
    }

    #[test]
    fn parses_scalars_and_edge_numbers() {
        assert_eq!(parse("null"), Some(Value::Null));
        assert_eq!(parse("  true  "), Some(Value::Bool(true)));
        assert_eq!(parse("\"\""), Some(Value::Str(String::new())));
        assert_eq!(parse("0").unwrap().as_i64(), Some(0));
        assert_eq!(parse("-0").unwrap().as_i64(), Some(0));
        assert_eq!(
            parse("123456789012").unwrap().as_i64(),
            Some(123_456_789_012)
        );
        assert_eq!(parse("[]"), Some(Value::Array(Vec::new())));
        assert_eq!(parse("{}"), Some(Value::Object(Vec::new())));
    }

    #[test]
    fn rejects_malformed_json() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\":}",
            "{\"a\" 1}",
            "nul",
            "01",           // leading zero
            "1.",           // no fraction digits
            "1e",           // no exponent digits
            "-",            // no digits
            "\"\\x\"",      // invalid escape
            "\"\u{0001}\"", // unescaped control character
            "truex",        // trailing garbage
            "[1, 2] []",    // trailing value
            "\"unterminated",
            "\"\\uD800\"", // a lone high surrogate
        ] {
            assert_eq!(parse(bad), None, "{bad:?} must be rejected");
        }
    }

    #[test]
    fn deep_nesting_is_refused_not_overflowed() {
        // A document nested well past the limit must return None, not overflow the stack.
        let deep = "[".repeat(10_000);
        assert_eq!(parse(&deep), None);
        // Just under the limit parses (an array of arrays … of an empty array).
        let ok = "[".repeat(MAX_DEPTH - 1) + &"]".repeat(MAX_DEPTH - 1);
        assert!(parse(&ok).is_some());
    }

    #[test]
    fn mutations_never_panic() {
        let base = br#"{"a":[1,2.5,true,null,"x\u00e9"],"b":{"c":-3e2}}"#;
        let mut rng = crate::testing::Rng::new(0x4A53_4F4E);
        for _ in 0..crate::testing::cases(20_000) {
            let mut data = base.to_vec();
            crate::testing::mutate(&mut data, &mut rng);
            if let Ok(s) = core::str::from_utf8(&data) {
                let _ = parse(s);
            }
        }
    }
}
