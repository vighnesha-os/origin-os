//! Test support: a seeded generator and byte mutators for the parser mutation tests
//! (CLAUDE.md §72: fuzzing; malicious-input handling). Deterministic, so a failure is
//! reproduced by its seed and case number; no fuzzing crate, so it runs on stable Rust
//! with `cargo test`.

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
        (self.next() % n as u64) as usize
    }

    pub(crate) fn byte(&mut self) -> u8 {
        self.next() as u8
    }
}

/// `default` cases, times `ORIGIN_MUTATION_SCALE` (default 1) for long soak runs.
pub(crate) fn cases(default: usize) -> usize {
    let scale = std::env::var("ORIGIN_MUTATION_SCALE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    default.saturating_mul(scale)
}

/// Values that sit on boundaries parsers get wrong.
const EDGES: [u8; 8] = [0x00, 0x01, 0x7F, 0x80, 0xFE, 0xFF, 0x20, 0x09];

/// Applies one to four random mutations to `data`: flip a bit, overwrite a byte with a
/// random or boundary value, truncate, extend, or copy a chunk elsewhere.
pub(crate) fn mutate(data: &mut Vec<u8>, rng: &mut Rng) {
    for _ in 0..1 + rng.below(4) {
        if data.is_empty() {
            data.push(rng.byte());
            continue;
        }
        let at = rng.below(data.len());
        match rng.below(7) {
            0 | 1 => data[at] ^= 1 << rng.below(8),
            2 => data[at] = rng.byte(),
            3 => data[at] = EDGES[rng.below(EDGES.len())],
            4 => data.truncate(at),
            5 => {
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
