//! UCS-2 encoding for `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL.OutputString` (UEFI 2.11 §12.4).
//!
//! `OutputString` takes a NUL-terminated `CHAR16` string. The loader builds those strings
//! in a fixed stack buffer, so printing never allocates.

/// Units per `OutputString` call, including the terminating NUL.
pub const CHUNK_UNITS: usize = 128;

/// U+FFFD REPLACEMENT CHARACTER.
const REPLACEMENT: u16 = 0xFFFD;

/// Accumulates units into NUL-terminated chunks and hands each full chunk to `emit`.
struct Chunker<E, F: FnMut(&[u16]) -> Result<(), E>> {
    buf: [u16; CHUNK_UNITS],
    len: usize,
    emit: F,
}

impl<E, F: FnMut(&[u16]) -> Result<(), E>> Chunker<E, F> {
    fn new(emit: F) -> Self {
        Self {
            buf: [0; CHUNK_UNITS],
            len: 0,
            emit,
        }
    }

    /// Appends `units` (at most two) without splitting them across chunks.
    fn push(&mut self, units: &[u16]) -> Result<(), E> {
        // Room for `units` plus the terminator.
        if self.len + units.len() >= CHUNK_UNITS {
            self.flush()?;
        }
        if let Some(dst) = self.buf.get_mut(self.len..self.len + units.len()) {
            dst.copy_from_slice(units);
            self.len += units.len();
        }
        Ok(())
    }

    /// Emits the pending units, if any, as one NUL-terminated chunk.
    fn flush(&mut self) -> Result<(), E> {
        if self.len == 0 {
            return Ok(());
        }
        let n = self.len;
        self.len = 0;
        match self.buf.get_mut(n) {
            Some(terminator) => *terminator = 0,
            None => return Ok(()),
        }
        match self.buf.get(..=n) {
            Some(chunk) => (self.emit)(chunk),
            None => Ok(()),
        }
    }
}

/// Encodes `s` for `OutputString` and passes it to `emit` as NUL-terminated chunks of at
/// most [`CHUNK_UNITS`] units.
///
/// - `'\n'` becomes `"\r\n"`, because UEFI consoles do not return the carriage on LF.
/// - Characters outside the Basic Multilingual Plane, which UCS-2 cannot represent, and
///   embedded NULs, which would end the string early, become U+FFFD.
///
/// Stops at the first error `emit` returns. An empty `s` emits nothing.
pub fn encode_str<E>(s: &str, emit: impl FnMut(&[u16]) -> Result<(), E>) -> Result<(), E> {
    let mut out = Chunker::new(emit);
    for c in s.chars() {
        match c {
            '\n' => out.push(&[0x000D, 0x000A])?,
            '\0' => out.push(&[REPLACEMENT])?,
            c => match u16::try_from(u32::from(c)) {
                Ok(unit) => out.push(&[unit])?,
                Err(_) => out.push(&[REPLACEMENT])?,
            },
        }
    }
    out.flush()
}

/// Passes already-encoded `CHAR16` units (for example a firmware string) to `emit` as
/// NUL-terminated chunks. A NUL unit inside `units` becomes U+FFFD.
pub fn encode_units<E>(units: &[u16], emit: impl FnMut(&[u16]) -> Result<(), E>) -> Result<(), E> {
    let mut out = Chunker::new(emit);
    for &u in units {
        out.push(&[if u == 0 { REPLACEMENT } else { u }])?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `encode_str` and returns each emitted chunk.
    fn chunks(s: &str) -> Vec<Vec<u16>> {
        let mut v = Vec::new();
        encode_str::<()>(s, |c| {
            v.push(c.to_vec());
            Ok(())
        })
        .unwrap();
        v
    }

    fn units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn ascii_is_one_terminated_chunk() {
        let mut want = units("Origin");
        want.push(0);
        assert_eq!(chunks("Origin"), vec![want]);
    }

    #[test]
    fn empty_string_emits_nothing() {
        assert!(chunks("").is_empty());
    }

    #[test]
    fn newline_becomes_crlf() {
        let mut want = units("a\r\nb");
        want.push(0);
        assert_eq!(chunks("a\nb"), vec![want]);
    }

    #[test]
    fn non_bmp_and_nul_become_replacement() {
        // U+1F600 is outside the BMP; U+0C24 (Telugu TA) is inside it.
        assert_eq!(
            chunks("\u{1F600}\u{0C24}\0"),
            vec![vec![0xFFFD, 0x0C24, 0xFFFD, 0]]
        );
    }

    #[test]
    fn long_text_splits_into_full_chunks() {
        let s = "x".repeat(300);
        let got = chunks(&s);
        // 127 units + NUL per chunk: 127 + 127 + 46.
        assert_eq!(
            got.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![128, 128, 47]
        );
        for c in &got {
            assert_eq!(c.last(), Some(&0));
            assert!(c[..c.len() - 1].iter().all(|&u| u == u16::from(b'x')));
        }
        let total: usize = got.iter().map(|c| c.len() - 1).sum();
        assert_eq!(total, 300);
    }

    #[test]
    fn crlf_is_never_split_across_chunks() {
        // 126 units, then "\n": CR+LF would need units 126 and 127, leaving no room for NUL.
        let s = format!("{}\n", "y".repeat(126));
        let got = chunks(&s);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].len(), 127);
        assert_eq!(got[1], vec![0x000D, 0x000A, 0]);
    }

    #[test]
    fn exact_fit_uses_one_chunk() {
        let got = chunks(&"z".repeat(CHUNK_UNITS - 1));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].len(), CHUNK_UNITS);
    }

    #[test]
    fn emit_error_stops_encoding() {
        let mut calls = 0;
        let r = encode_str(&"q".repeat(400), |_| {
            calls += 1;
            if calls == 2 {
                Err("device error")
            } else {
                Ok(())
            }
        });
        assert_eq!(r, Err("device error"));
        assert_eq!(calls, 2);
    }

    #[test]
    fn encode_units_replaces_nul() {
        let mut v = Vec::new();
        encode_units::<()>(&[0x45, 0, 0x46], |c| {
            v.push(c.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(v, vec![vec![0x45, 0xFFFD, 0x46, 0]]);
    }
}
