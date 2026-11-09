//! Tokens (`docs/spec/OIDL.md` §2). Reads the file as untrusted bytes: the size, the characters
//! and every limit are checked as they are met, and arithmetic cannot overflow.

use crate::diag::{Code, Diagnostic};

/// Largest input, in bytes (§2).
pub const MAX_SOURCE: usize = 1 << 20;
/// Most tokens in one file (§2).
pub const MAX_TOKENS: usize = 200_000;
/// Longest line, in bytes (§2).
pub const MAX_LINE: usize = 4096;
/// Longest identifier, in bytes (§2).
pub const MAX_IDENT: usize = 64;

/// Punctuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Punct {
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `,`
    Comma,
    /// `;`
    Semi,
    /// `:`
    Colon,
    /// `=`
    Eq,
    /// `.`
    Dot,
    /// `@`
    At,
    /// `|`
    Pipe,
    /// `-`
    Minus,
    /// `->`
    Arrow,
}

/// One token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// A word: an identifier or a contextual keyword.
    Ident(String),
    /// An unsigned number.
    Number(u64),
    /// A string.
    Str(String),
    /// A `///` comment's text.
    Doc(String),
    /// Punctuation.
    Punct(Punct),
    /// The end of the file.
    Eof,
}

/// A token and where it starts (1-based).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spanned {
    /// The token.
    pub token: Token,
    /// Line.
    pub line: u32,
    /// Column.
    pub column: u32,
}

struct Lexer<'a> {
    file: &'a str,
    src: &'a [u8],
    at: usize,
    line: u32,
    column: u32,
    line_start: usize,
    tokens: Vec<Spanned>,
}

impl Lexer<'_> {
    fn fail<T>(&self, code: Code, message: impl Into<String>) -> Result<T, Diagnostic> {
        Err(Diagnostic {
            code,
            file: self.file.to_string(),
            line: self.line,
            column: self.column,
            message: message.into(),
        })
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.at).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.src.get(self.at.saturating_add(ahead)).copied()
    }

    fn bump(&mut self) {
        self.at = self.at.saturating_add(1);
        self.column = self.column.saturating_add(1);
    }

    fn newline(&mut self) -> Result<(), Diagnostic> {
        if self.at.saturating_sub(self.line_start) > MAX_LINE {
            return self.fail(Code::LineTooLong, "the line ends here");
        }
        self.at = self.at.saturating_add(1);
        self.line = self.line.saturating_add(1);
        self.column = 1;
        self.line_start = self.at;
        Ok(())
    }

    fn push(&mut self, token: Token, line: u32, column: u32) -> Result<(), Diagnostic> {
        if self.tokens.len() >= MAX_TOKENS {
            return self.fail(Code::TooManyTokens, "too many tokens");
        }
        self.tokens.push(Spanned {
            token,
            line,
            column,
        });
        Ok(())
    }

    /// The text of a comment, up to the end of its line. Comments may hold any UTF-8 but
    /// control characters; the caller has checked the file is valid UTF-8.
    fn comment_text(&mut self) -> Result<String, Diagnostic> {
        let start = self.at;
        while let Some(c) = self.peek() {
            if c == b'\n' {
                break;
            }
            self.bump();
        }
        if self.at.saturating_sub(self.line_start) > MAX_LINE {
            return self.fail(Code::LineTooLong, "the line ends here");
        }
        let bytes = self.src.get(start..self.at).unwrap_or(&[]);
        let text = core::str::from_utf8(bytes).unwrap_or("");
        if text.chars().any(char::is_control) {
            return self.fail(
                Code::ForbiddenCharacter,
                "a comment contains a control character",
            );
        }
        Ok(text.to_string())
    }

    fn comment(&mut self) -> Result<(), Diagnostic> {
        let (line, column) = (self.line, self.column);
        // Three slashes and no fourth: a doc comment (§2).
        let is_doc = self.peek_at(2) == Some(b'/') && self.peek_at(3) != Some(b'/');
        let text = self.comment_text()?;
        if !is_doc {
            return Ok(());
        }
        // Trimmed, and refused if it holds a code fence: doc text must never become a doctest,
        // which `rustdoc --test` would run (§15 O11). An indented line would be a code block too.
        let body = text.strip_prefix("///").unwrap_or("").trim();
        if body.contains("*/") || body.contains("/*") {
            return self.fail(Code::ForbiddenDocText, "copied into a C comment");
        }
        if body.contains("```") || body.contains("~~~") {
            return self.fail(Code::ForbiddenDocText, "a code fence in a doc comment");
        }
        self.push(Token::Doc(body.to_string()), line, column)
    }

    fn number(&mut self) -> Result<Token, Diagnostic> {
        let hex = self.peek() == Some(b'0') && matches!(self.peek_at(1), Some(b'x' | b'X'));
        if hex {
            if self.peek_at(1) == Some(b'X') {
                return self.fail(Code::BadNumber, "hexadecimal numbers start with 0x");
            }
            self.bump();
            self.bump();
        }
        let radix: u32 = if hex { 16 } else { 10 };
        let mut value: u64 = 0;
        let mut digits = 0usize;
        let mut first_digit = None;
        let mut after_digit = false;
        loop {
            match self.peek() {
                Some(b'_') => {
                    if !after_digit {
                        return self.fail(Code::BadNumber, "_ must be between digits");
                    }
                    after_digit = false;
                    self.bump();
                }
                Some(c) if (c as char).is_digit(radix) => {
                    // A lower-case letter is a hex digit; an upper-case one is refused below.
                    let digit = u64::from((c as char).to_digit(radix).unwrap_or(0));
                    value = match value
                        .checked_mul(u64::from(radix))
                        .and_then(|v| v.checked_add(digit))
                    {
                        Some(v) => v,
                        None => return self.fail(Code::NumberTooLarge, "more than 64 bits"),
                    };
                    first_digit.get_or_insert(digit);
                    digits = digits.saturating_add(1);
                    after_digit = true;
                    self.bump();
                }
                _ => break,
            }
        }
        if digits == 0 || !after_digit {
            return self.fail(Code::BadNumber, "digits expected");
        }
        if !hex && digits > 1 && first_digit == Some(0) {
            return self.fail(Code::BadNumber, "a decimal number has no leading zero");
        }
        if matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_') {
            return self.fail(Code::BadNumber, "a number runs into a word");
        }
        Ok(Token::Number(value))
    }

    fn string(&mut self) -> Result<Token, Diagnostic> {
        self.bump();
        let start = self.at;
        loop {
            match self.peek() {
                Some(b'"') => break,
                Some(c) if (0x20..=0x7E).contains(&c) && c != b'\\' => self.bump(),
                _ => return self.fail(Code::BadString, "a string has printable ASCII only"),
            }
        }
        let bytes = self.src.get(start..self.at).unwrap_or(&[]);
        let text = core::str::from_utf8(bytes).unwrap_or("").to_string();
        self.bump();
        if text.len() > MAX_IDENT {
            return self.fail(Code::BadString, "a string is at most 64 bytes");
        }
        Ok(Token::Str(text))
    }

    fn word(&mut self) -> Result<Token, Diagnostic> {
        let start = self.at;
        while let Some(c) = self.peek() {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' {
                self.bump();
            } else {
                break;
            }
        }
        if matches!(self.peek(), Some(c) if c.is_ascii_uppercase()) {
            return self.fail(Code::BadIdentifier, "identifiers are lower-case");
        }
        let bytes = self.src.get(start..self.at).unwrap_or(&[]);
        let text = core::str::from_utf8(bytes).unwrap_or("").to_string();
        if text.len() > MAX_IDENT {
            return self.fail(Code::IdentifierTooLong, text);
        }
        if text.ends_with('_') || text.contains("__") {
            return self.fail(Code::BadIdentifier, text);
        }
        Ok(Token::Ident(text))
    }

    fn next_token(&mut self) -> Result<(), Diagnostic> {
        let (line, column) = (self.line, self.column);
        let Some(c) = self.peek() else {
            return Ok(());
        };
        let token = match c {
            b'/' if self.peek_at(1) == Some(b'/') => return self.comment(),
            b'0'..=b'9' => self.number()?,
            b'"' => self.string()?,
            b'a'..=b'z' => self.word()?,
            b' ' => {
                self.bump();
                return Ok(());
            }
            b'\n' => return self.newline(),
            b'-' if self.peek_at(1) == Some(b'>') => {
                self.bump();
                self.bump();
                Token::Punct(Punct::Arrow)
            }
            _ => {
                let punct = match c {
                    b'{' => Punct::LBrace,
                    b'}' => Punct::RBrace,
                    b'(' => Punct::LParen,
                    b')' => Punct::RParen,
                    b'<' => Punct::Lt,
                    b'>' => Punct::Gt,
                    b',' => Punct::Comma,
                    b';' => Punct::Semi,
                    b':' => Punct::Colon,
                    b'=' => Punct::Eq,
                    b'.' => Punct::Dot,
                    b'@' => Punct::At,
                    b'|' => Punct::Pipe,
                    b'-' => Punct::Minus,
                    b'A'..=b'Z' | b'_' => {
                        return self.fail(Code::BadIdentifier, "identifiers start with a-z");
                    }
                    _ => {
                        return self.fail(
                            Code::ForbiddenCharacter,
                            format!("byte {c:#04x} outside a comment"),
                        );
                    }
                };
                self.bump();
                Token::Punct(punct)
            }
        };
        self.push(token, line, column)
    }
}

/// The tokens of `source`, ending in [`Token::Eof`].
pub fn lex(file: &str, source: &[u8]) -> Result<Vec<Spanned>, Diagnostic> {
    let mut lexer = Lexer {
        file,
        src: source,
        at: 0,
        line: 1,
        column: 1,
        line_start: 0,
        tokens: Vec::new(),
    };
    if source.len() > MAX_SOURCE {
        return lexer.fail(Code::SourceTooLarge, format!("{} bytes", source.len()));
    }
    if core::str::from_utf8(source).is_err() {
        return lexer.fail(Code::NotUtf8, "");
    }
    while lexer.at < source.len() {
        lexer.next_token()?;
    }
    if lexer.at.saturating_sub(lexer.line_start) > MAX_LINE {
        return lexer.fail(Code::LineTooLong, "the last line");
    }
    let (line, column) = (lexer.line, lexer.column);
    lexer.push(Token::Eof, line, column)?;
    Ok(lexer.tokens)
}
