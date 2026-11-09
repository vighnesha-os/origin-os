//! Helpers shared by the known-answer test modules.

/// One gibibyte.
pub(crate) const GIB: u64 = 1 << 30;

/// The data lines of a vector file: everything except comments and blank lines.
pub(crate) fn data_lines(file: &'static str) -> impl Iterator<Item = &'static str> {
    file.lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
}

/// Bytes from hex; `-` stands for the empty string.
pub(crate) fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

pub(crate) fn hex(data: &[u8]) -> String {
    data.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `Ok` when `got` is the vector's answer, otherwise both, for the failure message.
pub(crate) fn same(got: &[u8], want: &[u8]) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("got {}, want {}", hex(got), hex(want)))
    }
}

/// The largest large-data vector to run, in bytes: `ORIGIN_KAT_SCALE` GiB, by default 1 GiB.
///
/// A value that is not a number fails the test instead of quietly selecting a smaller run.
pub(crate) fn large_data_limit() -> u64 {
    const DEFAULT_GIB: u64 = 1;
    let gib = match std::env::var("ORIGIN_KAT_SCALE") {
        Ok(value) => value
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("ORIGIN_KAT_SCALE must be a number of GiB, not {value:?}")),
        Err(_) => DEFAULT_GIB,
    };
    gib.saturating_mul(GIB)
}
