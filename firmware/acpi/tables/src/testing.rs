//! Test support: building tables, a seeded generator and byte mutators for the hostile-table
//! tests (CLAUDE.md §72). Deterministic, so a failure is reproduced by its seed and case number,
//! and with no fuzzing crate, so it runs on stable Rust under `cargo test` (CLAUDE.md §5).

/// A table with `signature`, `revision` and `body` after a 36-byte header, its length and checksum
/// set.
pub(crate) fn table(signature: &[u8; 4], revision: u8, body: &[u8]) -> Vec<u8> {
    let mut t = vec![0u8; 36];
    t[..4].copy_from_slice(signature);
    t[8] = revision;
    t[10..16].copy_from_slice(b"ORIGIN");
    t[16..24].copy_from_slice(b"TESTTBL ");
    t[24..28].copy_from_slice(&1u32.to_le_bytes());
    t[28..32].copy_from_slice(b"ORGN");
    t[32..36].copy_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(body);
    seal(&mut t);
    t
}

/// Sets a table's length field to its bytes and fixes its checksum: after a test edits a table,
/// so that the parser under test is reached past the header check.
pub(crate) fn seal(t: &mut [u8]) {
    if t.len() < 36 {
        return;
    }
    let len = u32::try_from(t.len()).unwrap();
    t[4..8].copy_from_slice(&len.to_le_bytes());
    t[9] = 0;
    let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);
}

/// Writes `value`'s little-endian bytes at `at`.
pub(crate) fn put(t: &mut [u8], at: usize, value: &[u8]) {
    t[at..at + value.len()].copy_from_slice(value);
}

/// A generic address structure (§5.2.3.2): space, bit width, bit offset, access size, address.
pub(crate) fn gas(space: u8, width: u8, offset: u8, access: u8, address: u64) -> [u8; 12] {
    let mut g = [0u8; 12];
    g[0] = space;
    g[1] = width;
    g[2] = offset;
    g[3] = access;
    g[4..].copy_from_slice(&address.to_le_bytes());
    g
}

/// xorshift64* (Vigna, 2016): small, fast and good enough to choose mutations.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..n` (`n` > 0).
    pub(crate) fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }

    pub(crate) fn byte(&mut self) -> u8 {
        self.next().to_le_bytes()[0]
    }

    pub(crate) fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    /// A value drawn towards where parsers go wrong: zero, one, all ones, a power of two or its
    /// neighbours, a low mask, or a uniform value.
    pub(crate) fn edge_u64(&mut self) -> u64 {
        let bit = 1u64 << self.below(64);
        match self.below(8) {
            0 => 0,
            1 => 1,
            2 => u64::MAX,
            3 => bit,
            4 => bit.wrapping_sub(1),
            5 => bit.wrapping_add(1),
            6 => u64::MAX >> self.below(64),
            _ => self.next(),
        }
    }

    /// A length or offset drawn towards the boundaries of a run of `len` bytes: 0, 1, the end
    /// and its neighbours, small values, the widths' maxima, or anything.
    pub(crate) fn edge_len(&mut self, len: usize) -> u64 {
        let len = u64::try_from(len).unwrap();
        match self.below(10) {
            0 => 0,
            1 => 1,
            2 => len,
            3 => len.wrapping_sub(1),
            4 => len.wrapping_add(1),
            5 => u64::from(u8::MAX),
            6 => u64::from(u16::MAX),
            7 => u64::from(u32::MAX),
            8 => u64::try_from(self.below(64)).unwrap(),
            _ => self.edge_u64(),
        }
    }
}

/// `default` cases, times `ORIGIN_MUTATION_SCALE` (default 1) for long soaks.
pub(crate) fn cases(default: usize) -> usize {
    let scale = std::env::var("ORIGIN_MUTATION_SCALE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    default.saturating_mul(scale)
}

/// Bytes that sit on boundaries parsers get wrong.
const EDGES: [u8; 8] = [0x00, 0x01, 0x7F, 0x80, 0xFE, 0xFF, 0x02, 0x24];

/// Applies one to four mutations to `data`: flip a bit, overwrite a byte with a random or boundary
/// value, write a boundary word, truncate, extend, or copy a chunk elsewhere.
pub(crate) fn mutate(data: &mut Vec<u8>, rng: &mut Rng) {
    for _ in 0..1 + rng.below(4) {
        if data.is_empty() {
            data.push(rng.byte());
            continue;
        }
        let at = rng.below(data.len());
        match rng.below(8) {
            0 | 1 => data[at] ^= 1 << rng.below(8),
            2 => data[at] = rng.byte(),
            3 => data[at] = EDGES[rng.below(EDGES.len())],
            4 => {
                let word = rng.edge_len(data.len()).to_le_bytes();
                let width = [1usize, 2, 4, 8][rng.below(4)];
                let end = (at + width).min(data.len());
                data[at..end].copy_from_slice(&word[..end - at]);
            }
            5 => data.truncate(at),
            6 => {
                for _ in 0..1 + rng.below(16) {
                    data.push(rng.byte());
                }
            }
            _ => {
                let len = 1 + rng.below(16.min(data.len()));
                let from = rng.below(data.len() - len + 1);
                let to = rng.below(data.len() - len + 1);
                let chunk = data[from..from + len].to_vec();
                data[to..to + len].copy_from_slice(&chunk);
            }
        }
    }
}

#[test]
fn the_generator_is_deterministic_and_spreads() {
    let mut a = Rng::new(7);
    let mut b = Rng::new(7);
    let xs: Vec<u64> = (0..100).map(|_| a.next()).collect();
    assert!(xs.iter().all(|&x| x == b.next()));
    let mut seen = [0u32; 8];
    let mut r = Rng::new(1);
    for _ in 0..8000 {
        seen[r.below(8)] += 1;
    }
    assert!(seen.iter().all(|&n| (800..1200).contains(&n)), "{seen:?}");
}

#[test]
fn a_sealed_table_checks() {
    let mut t = table(b"TEST", 1, &[1, 2, 3]);
    t.push(9);
    seal(&mut t);
    assert!(crate::header::check(&t).is_ok());
}
