//! Turning a captured QEMU serial stream into plain text, and matching expected output.

/// Removes terminal control sequences and carriage returns from raw serial bytes.
///
/// OVMF's serial terminal emits ECMA-48 control sequences (`ESC [ … final`, including
/// private forms such as `ESC [ = 3 h`), two-byte `ESC x` sequences, and CR before LF.
/// The result is lossy UTF-8.
pub(crate) fn clean(raw: &[u8]) -> String {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while let Some(&b) = raw.get(i) {
        match b {
            0x1B => {
                i += 1;
                if raw.get(i) == Some(&b'[') {
                    i += 1;
                    // Parameter (0x30-0x3F) and intermediate (0x20-0x2F) bytes, then one final byte.
                    while raw.get(i).is_some_and(|&c| (0x20..=0x3F).contains(&c)) {
                        i += 1;
                    }
                    if raw.get(i).is_some_and(|&c| (0x40..=0x7E).contains(&c)) {
                        i += 1;
                    }
                } else if raw.get(i).is_some() {
                    i += 1; // ESC plus one byte
                }
            }
            b'\r' => i += 1,
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True if every marker appears in `text`, each one after the end of the previous match.
pub(crate) fn contains_in_order(text: &str, markers: &[&str]) -> bool {
    let mut rest = text;
    for m in markers {
        match rest.find(m) {
            Some(at) => rest = rest.get(at + m.len()..).unwrap_or(""),
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_private_and_crlf() {
        let raw = b"\x1b[=3h\x1b[2J\x1b[01;01HBdsDxe: starting\r\nOrigin\x1b[0m OS\r\n";
        assert_eq!(clean(raw), "BdsDxe: starting\nOrigin OS\n");
    }

    #[test]
    fn strips_two_byte_escape_and_survives_truncation() {
        assert_eq!(clean(b"a\x1bcb"), "ab");
        assert_eq!(clean(b"tail\x1b[1;2"), "tail");
        assert_eq!(clean(b"end\x1b"), "end");
    }

    #[test]
    fn order_matters() {
        let t = "one\ntwo\nthree\n";
        assert!(contains_in_order(t, &["one", "two", "three"]));
        assert!(!contains_in_order(t, &["two", "one"]));
        assert!(!contains_in_order(t, &["one", "four"]));
        assert!(contains_in_order(t, &[]));
    }
}
