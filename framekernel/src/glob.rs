//! Glob pattern matching for path and name filtering (CLAUDE.md §39 the VFS; used by a
//! shell, a package file list or a search). A small, deterministic matcher over Unicode
//! scalars with the usual POSIX-ish syntax:
//!
//! - `*` matches any run of characters **except** `/`,
//! - `?` matches exactly one character except `/`,
//! - `[...]` matches one character from a set — ranges (`a-z`), and negation with a leading
//!   `!` or `^`; a `]` right after the `[` (or `[!`) is a literal `]`,
//! - `\` escapes the next character (so `\*` is a literal `*`),
//! - every other character, `/` included, matches itself.
//!
//! Because the wildcards never cross `/`, one pattern matches within a path segment unless
//! it spells the separators out (`src/*.rs`); there is no `**` yet. Pure, no `unsafe`.

extern crate alloc;
use alloc::vec::Vec;

/// One member of a `[...]` class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClassItem {
    Char(char),
    Range(char, char),
}

/// One compiled pattern token.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Literal(char),
    AnyOne,
    AnyRun,
    Class { negate: bool, items: Vec<ClassItem> },
}

impl Token {
    /// Whether this single-character token matches `c`. `/` is never matched by a wildcard
    /// or class, only by a literal `/`.
    fn matches(&self, c: char) -> bool {
        match self {
            Token::Literal(l) => *l == c,
            Token::AnyOne => c != '/',
            Token::AnyRun => c != '/', // handled specially by the matcher, but guards `/`
            Token::Class { negate, items } => {
                if c == '/' {
                    return false;
                }
                let hit = items.iter().any(|item| match item {
                    ClassItem::Char(x) => *x == c,
                    ClassItem::Range(lo, hi) => *lo <= c && c <= *hi,
                });
                hit != *negate
            }
        }
    }
}

/// Parses a `[...]` class starting at `chars[0] == '['`. Returns the token and how many
/// characters it consumed, or `None` if the class is unterminated (the caller then treats
/// `[` as a literal).
fn parse_class(chars: &[char]) -> Option<(Token, usize)> {
    let mut i = 1; // past '['
    let negate = matches!(chars.get(i), Some('!' | '^'));
    if negate {
        i += 1;
    }
    let mut items = Vec::new();
    // A ']' in the first position is a literal member, not the terminator.
    let mut first = true;
    while let Some(&c) = chars.get(i) {
        if c == ']' && !first {
            return Some((Token::Class { negate, items }, i + 1));
        }
        first = false;
        // An escape inside the class makes the next character a literal member.
        let (ch, step) = if c == '\\' {
            match chars.get(i + 1) {
                Some(&e) => (e, 2),
                None => return None, // trailing backslash, no close
            }
        } else {
            (c, 1)
        };
        // A range `a-z`, unless the '-' is the last char before ']'.
        if chars.get(i + step) == Some(&'-') && chars.get(i + step + 1).is_some_and(|&n| n != ']') {
            let hi = chars[i + step + 1];
            items.push(ClassItem::Range(ch, hi));
            i += step + 2;
        } else {
            items.push(ClassItem::Char(ch));
            i += step;
        }
    }
    None // reached the end without a closing ']'
}

/// Compiles `pattern` into tokens.
fn compile(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                // Collapse runs of '*' into one.
                if out.last() != Some(&Token::AnyRun) {
                    out.push(Token::AnyRun);
                }
                i += 1;
            }
            '?' => {
                out.push(Token::AnyOne);
                i += 1;
            }
            '\\' => {
                if let Some(&next) = chars.get(i + 1) {
                    out.push(Token::Literal(next));
                    i += 2;
                } else {
                    out.push(Token::Literal('\\')); // trailing backslash is literal
                    i += 1;
                }
            }
            '[' => match parse_class(&chars[i..]) {
                Some((tok, consumed)) => {
                    out.push(tok);
                    i += consumed;
                }
                None => {
                    out.push(Token::Literal('['));
                    i += 1;
                }
            },
            c => {
                out.push(Token::Literal(c));
                i += 1;
            }
        }
    }
    out
}

/// Whether `text` matches the glob `pattern`.
pub fn matches(pattern: &str, text: &str) -> bool {
    let tokens = compile(pattern);
    let text: Vec<char> = text.chars().collect();
    let mut ti = 0; // token index
    let mut si = 0; // scalar index into text
    // The last `*` we passed, and the text position it currently consumes up to, for
    // backtracking: `*` may extend one more scalar as long as that scalar is not `/`.
    let mut star: Option<usize> = None;
    let mut star_si = 0;

    loop {
        if si < text.len() {
            match tokens.get(ti) {
                Some(Token::AnyRun) => {
                    star = Some(ti);
                    star_si = si;
                    ti += 1;
                    continue;
                }
                Some(tok) if tok.matches(text[si]) => {
                    ti += 1;
                    si += 1;
                    continue;
                }
                _ => {}
            }
        } else {
            // Text consumed: the pattern matches iff only trailing `*` remain.
            while tokens.get(ti) == Some(&Token::AnyRun) {
                ti += 1;
            }
            return ti == tokens.len();
        }
        // Mismatch: extend the most recent `*` by one scalar, if it may cross this one.
        if let Some(stok) = star
            && text[star_si] != '/'
        {
            star_si += 1;
            si = star_si;
            ti = stok + 1;
            continue;
        }
        return false;
    }
}

/// Filters `names`, keeping those that match `pattern`, into `out`.
pub fn filter<'a>(pattern: &str, names: impl IntoIterator<Item = &'a str>, out: &mut Vec<&'a str>) {
    for name in names {
        if matches(pattern, name) {
            out.push(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_single_character() {
        assert!(matches("hello", "hello"));
        assert!(!matches("hello", "hell"));
        assert!(!matches("hello", "hello!"));
        assert!(matches("h?llo", "hello"));
        assert!(matches("h?llo", "hallo"));
        assert!(!matches("h?llo", "hllo"), "? needs exactly one");
        assert!(!matches("h?llo", "h/llo"), "? never matches /");
        assert!(matches("", ""));
        assert!(!matches("", "x"));
    }

    #[test]
    fn stars_match_runs_but_not_slash() {
        assert!(matches("*", "anything"));
        assert!(matches("*", ""));
        assert!(matches("*.rs", "main.rs"));
        assert!(matches("*.rs", ".rs"));
        assert!(!matches("*.rs", "main.rsx"));
        assert!(matches("a*b*c", "a__b____c"));
        assert!(!matches("a*b*c", "a__b____d"));
        // A '*' does not cross '/'.
        assert!(!matches("*.rs", "src/main.rs"));
        assert!(matches("src/*.rs", "src/main.rs"));
        assert!(!matches("src/*.rs", "src/sub/main.rs"));
        assert!(matches("src/*/*.rs", "src/sub/main.rs"));
        // Collapsed stars behave like one.
        assert!(matches("a***b", "axyzb"));
    }

    #[test]
    fn character_classes() {
        assert!(matches("[abc]", "b"));
        assert!(!matches("[abc]", "d"));
        assert!(matches("file[0-9].log", "file7.log"));
        assert!(!matches("file[0-9].log", "fileX.log"));
        assert!(matches("[a-cx-z]", "y"));
        assert!(!matches("[a-cx-z]", "m"));
        // Negation with ! or ^.
        assert!(matches("[!0-9]", "a"));
        assert!(!matches("[!0-9]", "5"));
        assert!(matches("[^abc]", "d"));
        assert!(!matches("[^abc]", "a"));
        // A class never matches '/'.
        assert!(
            !matches("[a-z/]", "/"),
            "a literal / inside a class still excluded"
        );
        // A ']' right after '[' is a literal member.
        assert!(matches("[]x]", "]"));
        assert!(matches("[]x]", "x"));
        // A trailing '-' is a literal.
        assert!(matches("[a-]", "-"));
        assert!(matches("[a-]", "a"));
        // An unterminated class is a literal '['.
        assert!(matches("[abc", "[abc"));
        assert!(!matches("[abc", "a"));
    }

    #[test]
    fn escaping() {
        assert!(matches("\\*", "*"));
        assert!(!matches("\\*", "x"));
        assert!(matches("a\\?b", "a?b"));
        assert!(!matches("a\\?b", "axb"));
        assert!(matches("\\[x\\]", "[x]"));
        // A backslash inside a class escapes the next member.
        assert!(matches("[\\]]", "]"));
    }

    #[test]
    fn unicode_and_filtering() {
        assert!(matches("caf?", "café"));
        assert!(matches("*é", "café"));
        assert!(matches("[α-ω]", "δ"), "Greek range");
        assert!(!matches("[α-ω]", "Δ"), "uppercase excluded");

        let names = ["main.rs", "lib.rs", "notes.txt", "src/x.rs"];
        let mut out = Vec::new();
        filter("*.rs", names, &mut out);
        assert_eq!(
            out,
            ["main.rs", "lib.rs"],
            "src/x.rs excluded: * stops at /"
        );
    }

    /// The star-backtracking must not blow up or mismatch on adversarial inputs: many stars
    /// against a long non-matching string still terminates with the right answer.
    #[test]
    fn pathological_backtracking_terminates() {
        let text = "a".repeat(64);
        assert!(!matches("*a*a*a*a*b", &text));
        assert!(matches("*a*a*a*a*a", &text));
        assert!(matches(&"*".repeat(20), &text));
        // The classic worst case for naive matchers.
        assert!(!matches("a*a*a*a*a*a*a*a*b", &"a".repeat(32)));
    }
}
