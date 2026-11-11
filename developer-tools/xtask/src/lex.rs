//! A small lexer for Rust source, as much as xtask's gates need: it tells code from comments
//! and from string and character literals, and hands back identifiers, punctuation and string
//! literals with their places. It does not parse Rust.
//!
//! The gates built on it count `unsafe` (`unsafety`), read assembly blocks (`asmflags`) and
//! read `cfg` predicates (`targetcfg`). A token it gets wrong can only come from source the
//! compiler would refuse too: each gate runs after the tree has compiled.

/// What a [`Token`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// An identifier or a keyword.
    Ident,
    /// A raw identifier, `r#name`: never a keyword. The token's text is `name`.
    RawIdent,
    /// A string literal of any flavour: `"…"`, `b"…"`, `c"…"` and their raw forms. The token's
    /// text is what lies between the quotes, not decoded ([`unescape`] decodes it).
    Str {
        /// Whether it is a raw string, in which a backslash is only a backslash.
        raw: bool,
    },
    /// A character or byte literal, quotes included.
    Char,
    /// A number, with its suffix if it has one.
    Number,
    /// One ASCII punctuation character.
    Punct,
}

/// One token of the source, outside every comment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Token<'a> {
    pub(crate) kind: Kind,
    pub(crate) text: &'a str,
    /// Where the token starts in the source, in bytes.
    pub(crate) start: usize,
}

impl Token<'_> {
    /// Whether this is the identifier or keyword `word`.
    pub(crate) fn is_ident(&self, word: &str) -> bool {
        self.kind == Kind::Ident && self.text == word
    }

    /// Whether this is the punctuation character `mark`.
    pub(crate) fn is_punct(&self, mark: char) -> bool {
        self.kind == Kind::Punct && self.text.chars().eq([mark])
    }
}

/// The tokens of `source`, in order. Comments and white space produce none.
pub(crate) fn tokens(source: &str) -> Vec<Token<'_>> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut push = |kind, from: usize, to: usize, start: usize| {
        // Every range below starts and ends next to an ASCII byte, so it is a valid slice.
        if let Some(text) = source.get(from..to) {
            out.push(Token { kind, text, start });
        }
    };
    while let Some(&byte) = bytes.get(i) {
        match byte {
            b'/' if bytes.get(i + 1) == Some(&b'/') => i = line_end(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = block_comment_end(bytes, i + 2),
            b'"' => {
                let (body_end, end) = string_end(bytes, i + 1);
                push(Kind::Str { raw: false }, i + 1, body_end, i);
                i = end;
            }
            b'\'' => {
                let end = char_or_lifetime_end(bytes, i);
                if end > i + 1 {
                    push(Kind::Char, i, end, i);
                } else {
                    // A lifetime or a label: its name follows as an identifier.
                    push(Kind::Punct, i, i + 1, i);
                }
                i = end;
            }
            b'_' | b'a'..=b'z' | b'A'..=b'Z' => {
                let end = identifier_end(bytes, i);
                let word = source.get(i..end).unwrap_or("");
                match word {
                    "r" | "br" | "cr" if raw_string_start(bytes, end).is_some() => {
                        let (body, body_end, after) = raw_string(bytes, end);
                        push(Kind::Str { raw: true }, body, body_end, i);
                        i = after;
                    }
                    "b" | "c" if bytes.get(end) == Some(&b'"') => {
                        let (body_end, after) = string_end(bytes, end + 1);
                        push(Kind::Str { raw: false }, end + 1, body_end, i);
                        i = after;
                    }
                    "b" if bytes.get(end) == Some(&b'\'') => {
                        let after = char_or_lifetime_end(bytes, end);
                        push(Kind::Char, i, after, i);
                        i = after;
                    }
                    "r" if bytes.get(end) == Some(&b'#') => {
                        // A raw identifier: `r#unsafe` names an ordinary identifier.
                        let after = identifier_end(bytes, end + 1);
                        push(Kind::RawIdent, end + 1, after, i);
                        i = after;
                    }
                    _ => {
                        push(Kind::Ident, i, end, i);
                        i = end;
                    }
                }
            }
            b'0'..=b'9' => {
                let end = identifier_end(bytes, i);
                push(Kind::Number, i, end, i);
                i = end;
            }
            _ if byte.is_ascii_whitespace() => i += 1,
            _ if byte.is_ascii() => {
                push(Kind::Punct, i, i + 1, i);
                i += 1;
            }
            // A character outside ASCII, in code: nothing the gates look for. Skip all of it.
            _ => i += utf8_len(byte),
        }
    }
    out
}

/// `Some(1)` for an opening bracket of any kind, `Some(-1)` for a closing one.
pub(crate) fn bracket(token: &Token<'_>) -> Option<i32> {
    if token.kind != Kind::Punct {
        return None;
    }
    match token.text {
        "(" | "[" | "{" => Some(1),
        ")" | "]" | "}" => Some(-1),
        _ => None,
    }
}

/// The index of the bracket that closes the one at `open`, or `None` if nothing does.
pub(crate) fn closing(tokens: &[Token<'_>], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        depth += bracket(token).unwrap_or(0);
        if depth <= 0 {
            return (depth == 0).then_some(i);
        }
    }
    None
}

/// The line `offset` is on, counting from 1.
pub(crate) fn line_of(source: &str, offset: usize) -> usize {
    let upto = offset.min(source.len());
    1 + source
        .as_bytes()
        .iter()
        .take(upto)
        .filter(|&&b| b == b'\n')
        .count()
}

/// What a string literal that is not raw stands for, from the text between its quotes, or
/// `None` if it holds an escape this does not know.
pub(crate) fn unescape(body: &str) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'x' => {
                let high = chars.next()?.to_digit(16)?;
                let low = chars.next()?.to_digit(16)?;
                out.push(char::from_u32(high * 16 + low)?);
            }
            'u' => {
                if chars.next()? != '{' {
                    return None;
                }
                let mut value = 0u32;
                loop {
                    match chars.next()? {
                        '}' => break,
                        '_' => {}
                        digit => value = value.checked_mul(16)?.checked_add(digit.to_digit(16)?)?,
                    }
                }
                out.push(char::from_u32(value)?);
            }
            // A backslash at the end of a line: the line break and the white space that opens
            // the next line are not part of the string.
            '\n' => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

fn is_identifier_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

/// The index just after the identifier (or number) that starts at `start`.
fn identifier_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while bytes.get(end).copied().is_some_and(is_identifier_byte) {
        end += 1;
    }
    end
}

/// How many bytes the UTF-8 character with this first byte takes.
fn utf8_len(first: u8) -> usize {
    match first {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

fn line_end(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(|&b| b != b'\n') {
        i += 1;
    }
    i
}

/// The index after the nested block comment whose body starts at `i` (after `/*`).
fn block_comment_end(bytes: &[u8], mut i: usize) -> usize {
    let mut depth = 1;
    while depth > 0 && i < bytes.len() {
        if bytes.get(i) == Some(&b'/') && bytes.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if bytes.get(i) == Some(&b'*') && bytes.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    i
}

/// For the string whose body starts at `i` (after its opening quote): where the body ends, and
/// the index after the closing quote. A string the file ends inside runs to the end of the file.
fn string_end(bytes: &[u8], mut i: usize) -> (usize, usize) {
    while let Some(&byte) = bytes.get(i) {
        match byte {
            b'\\' => i += 2,
            b'"' => return (i, i + 1),
            _ => i += 1,
        }
    }
    (bytes.len(), bytes.len())
}

/// The hash count if `at` starts a raw string's delimiter (`#*"`).
fn raw_string_start(bytes: &[u8], at: usize) -> Option<usize> {
    let mut hashes = 0;
    while bytes.get(at + hashes) == Some(&b'#') {
        hashes += 1;
    }
    (bytes.get(at + hashes) == Some(&b'"')).then_some(hashes)
}

/// For the raw string whose delimiter starts at `at`: where its body starts and ends, and the
/// index after its closing delimiter.
fn raw_string(bytes: &[u8], at: usize) -> (usize, usize, usize) {
    let hashes = raw_string_start(bytes, at).unwrap_or(0);
    let body = at + hashes + 1;
    let mut i = body;
    while i < bytes.len() {
        if bytes.get(i) == Some(&b'"') && (0..hashes).all(|k| bytes.get(i + 1 + k) == Some(&b'#')) {
            return (body, i, i + 1 + hashes);
        }
        i += 1;
    }
    (body.min(bytes.len()), bytes.len(), bytes.len())
}

/// The index after a character literal, or after the `'` if this is a lifetime. A character
/// literal is either escaped (`'\…'`) or holds exactly one character before its closing quote.
fn char_or_lifetime_end(bytes: &[u8], at: usize) -> usize {
    let start = at + 1;
    match bytes.get(start) {
        Some(b'\\') => {
            let mut i = start + 2;
            // Skip to the closing quote: an escape may be longer than one byte.
            while let Some(&byte) = bytes.get(i) {
                if byte == b'\'' {
                    return i + 1;
                }
                i += 1;
            }
            i
        }
        Some(&first) => {
            let close = start + utf8_len(first);
            if bytes.get(close) == Some(&b'\'') {
                close + 1
            } else {
                start
            }
        }
        None => start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<(Kind, &str)> {
        tokens(source).iter().map(|t| (t.kind, t.text)).collect()
    }

    #[test]
    fn code_becomes_identifiers_numbers_and_punctuation() {
        assert_eq!(
            kinds("let x1 = 0x1F_u8;"),
            [
                (Kind::Ident, "let"),
                (Kind::Ident, "x1"),
                (Kind::Punct, "="),
                (Kind::Number, "0x1F_u8"),
                (Kind::Punct, ";"),
            ]
        );
    }

    #[test]
    fn comments_produce_nothing_and_strings_one_token_each() {
        let source = r##"
            // a line comment with "a string" and 'c'
            /* a block, /* nested */ still a comment */
            a "one \" two" b"bytes" c"c" r"raw\" r#"raw "quoted""# br#"x"# d
        "##;
        assert_eq!(
            kinds(source),
            [
                (Kind::Ident, "a"),
                (Kind::Str { raw: false }, r#"one \" two"#),
                (Kind::Str { raw: false }, "bytes"),
                (Kind::Str { raw: false }, "c"),
                (Kind::Str { raw: true }, r"raw\"),
                (Kind::Str { raw: true }, r#"raw "quoted""#),
                (Kind::Str { raw: true }, "x"),
                (Kind::Ident, "d"),
            ]
        );
    }

    #[test]
    fn a_lifetime_is_not_a_character_literal() {
        // Two lifetimes three bytes apart are not one literal holding `a, `.
        assert_eq!(
            kinds("<'a, 'b> 'x' '\\'' b'y' '\u{e9}'"),
            [
                (Kind::Punct, "<"),
                (Kind::Punct, "'"),
                (Kind::Ident, "a"),
                (Kind::Punct, ","),
                (Kind::Punct, "'"),
                (Kind::Ident, "b"),
                (Kind::Punct, ">"),
                (Kind::Char, "'x'"),
                (Kind::Char, "'\\''"),
                (Kind::Char, "b'y'"),
                (Kind::Char, "'\u{e9}'"),
            ]
        );
    }

    #[test]
    fn a_raw_identifier_is_not_a_keyword() {
        assert_eq!(kinds("r#unsafe"), [(Kind::RawIdent, "unsafe")]);
        assert!(!tokens("r#unsafe")[0].is_ident("unsafe"));
        assert!(tokens("unsafe")[0].is_ident("unsafe"));
    }

    #[test]
    fn text_outside_ascii_is_skipped_whole_and_a_file_may_end_anywhere() {
        assert_eq!(
            kinds("a \u{2014} b"),
            [(Kind::Ident, "a"), (Kind::Ident, "b")]
        );
        for cut in ["\"open", "r#\"open", "/* open", "'", "b'", "'\\"] {
            // No panic and no token past the end.
            let _ = tokens(cut);
        }
        assert_eq!(kinds("\"open"), [(Kind::Str { raw: false }, "open")]);
    }

    #[test]
    fn a_bracket_finds_the_one_that_closes_it() {
        let found = tokens("f(a[0], \"(\", ')') { x }");
        assert_eq!(closing(&found, 1), Some(10));
        assert_eq!(closing(&found, 3), Some(5));
        assert_eq!(closing(&found, 11), Some(13));
        // Nothing closes the last one, and a token that opens nothing closes at once or not at
        // all.
        assert_eq!(closing(&tokens("f(a"), 1), None);
        assert_eq!(closing(&tokens(") x"), 0), None);
    }

    #[test]
    fn a_token_knows_its_line() {
        let source = "a\n\n  b";
        let found = tokens(source);
        assert_eq!(line_of(source, found[0].start), 1);
        assert_eq!(line_of(source, found[1].start), 3);
        assert_eq!(line_of(source, usize::MAX), 3);
    }

    #[test]
    fn escapes_are_decoded_and_unknown_ones_refused() {
        assert_eq!(
            unescape(r#"a\n\t\\\"\'\0\x41\u{1F600}"#).as_deref(),
            Some("a\n\t\\\"'\0A\u{1F600}")
        );
        // A backslash before a line break drops the break and the next line's indentation.
        assert_eq!(unescape("mov\\\n      rax").as_deref(), Some("movrax"));
        for bad in [r"\q", r"\x4", r"\u{110000}", r"\u41", "\\"] {
            assert_eq!(unescape(bad), None, "{bad}");
        }
    }
}
