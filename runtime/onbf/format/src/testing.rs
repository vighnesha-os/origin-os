//! Test support for the ONBF mutation soaks (CLAUDE.md §72; ONBF §13 item 3): a seeded
//! generator and byte mutators. Deterministic, so a failure is reproduced by its seed and
//! case number; no fuzzing crate (§5), so it runs on stable Rust under `cargo test`.
//!
//! Test-only. `onbf-merkle` and `onbf-verify` include this file by path, so there is one
//! copy and no dependency, feature or new crate for test code; not every crate uses every
//! function, which is why dead code is allowed here and nowhere else.
#![allow(dead_code)]

/// xorshift64* (Vigna, 2016): small, fast and good enough to choose mutations.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// The generator for one case of a soak, so that a failure is reproduced from its seed
    /// and case number alone, whatever earlier cases drew.
    pub(crate) fn for_case(seed: u64, case: usize) -> Self {
        let mut rng = Self::new(seed ^ (case as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        rng.next();
        rng
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

    /// True `percent` times in a hundred.
    pub(crate) fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub(crate) fn byte(&mut self) -> u8 {
        self.next() as u8
    }

    /// One of `items`.
    pub(crate) fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    /// A 16-bit value biased to where arithmetic goes wrong.
    pub(crate) fn edge_u16(&mut self) -> u16 {
        self.edge_u64() as u16
    }

    /// A 32-bit value biased to where arithmetic goes wrong: zero, one, all ones, powers of
    /// two and their neighbours, low masks, or a uniform value. A uniform draw almost never
    /// produces `u32::MAX`, which is the value a missing overflow check needs.
    pub(crate) fn edge_u32(&mut self) -> u32 {
        self.edge_u64() as u32
    }

    /// As [`Rng::edge_u32`], over 64 bits.
    pub(crate) fn edge_u64(&mut self) -> u64 {
        let bit = 1u64 << self.below(64);
        match self.below(8) {
            0 => 0,
            1 => 1,
            2 => u64::MAX,
            3 => bit,
            4 => bit.wrapping_sub(1),
            5 => bit.wrapping_add(1),
            // A low mask, so a 32-bit or 16-bit field can be all ones without the rest.
            6 => u64::MAX >> self.below(64),
            _ => self.next(),
        }
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

/// [`mutate`] confined to `window` of a fixed-size record, which keeps its length: for a
/// structure whose fields sit in a small part of a larger input, where uniform mutations
/// would rarely reach them.
pub(crate) fn mutate_window(data: &mut [u8], window: core::ops::Range<usize>, rng: &mut Rng) {
    let mut part = data[window.clone()].to_vec();
    mutate(&mut part, rng);
    part.resize(window.len(), 0);
    data[window].copy_from_slice(&part);
}

/// How often each named outcome of a soak happened: the evidence that the soak reaches what
/// it claims to.
#[derive(Default)]
pub(crate) struct Reached(std::collections::BTreeMap<&'static str, u64>);

impl Reached {
    pub(crate) fn hit(&mut self, name: &'static str) {
        *self.0.entry(name).or_insert(0) += 1;
    }

    pub(crate) fn count(&self, name: &str) -> u64 {
        self.0.get(name).copied().unwrap_or(0)
    }

    /// Fails unless every name in `required` was reached. Prints what was reached when
    /// `ORIGIN_MUTATION_REPORT` is set (the soak's coverage, for the provenance record).
    pub(crate) fn require(&self, soak: &str, required: &[&str]) {
        if std::env::var_os("ORIGIN_MUTATION_REPORT").is_some() {
            println!("{soak}: {:?}", self.0);
        }
        let missing: Vec<&&str> = required.iter().filter(|n| self.count(n) == 0).collect();
        assert!(
            missing.is_empty(),
            "{soak}: never reached {missing:?}; reached: {:?}",
            self.0
        );
    }
}

/// The soaks' "no arithmetic overflow" claim holds only where overflow checks are on, which
/// the test profile does at any opt-level.
#[cfg(debug_assertions)]
#[test]
fn overflow_checks_are_on_in_this_build() {
    let overflowed =
        std::panic::catch_unwind(|| std::hint::black_box(u32::MAX) + std::hint::black_box(1));
    assert!(overflowed.is_err(), "overflow checks are off");
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
fn a_case_generator_depends_only_on_seed_and_case() {
    let draw = |seed, case| Rng::for_case(seed, case).next();
    assert_eq!(draw(5, 3), draw(5, 3));
    assert_ne!(draw(5, 3), draw(5, 4));
    assert_ne!(draw(5, 3), draw(6, 3));
}

#[test]
fn edge_values_reach_the_extremes() {
    let mut rng = Rng::new(9);
    let mut seen = [false; 4];
    for _ in 0..4000 {
        match rng.edge_u32() {
            0 => seen[0] = true,
            1 => seen[1] = true,
            u32::MAX => seen[2] = true,
            0x8000_0000 => seen[3] = true,
            _ => {}
        }
    }
    assert_eq!(seen, [true; 4]);
}

#[test]
fn a_window_mutation_stays_in_its_window() {
    let mut rng = Rng::new(11);
    for _ in 0..2000 {
        let mut data = [0x55u8; 64];
        mutate_window(&mut data, 8..16, &mut rng);
        assert!(data[..8].iter().chain(&data[16..]).all(|&b| b == 0x55));
    }
}
