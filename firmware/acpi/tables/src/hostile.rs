//! Hostile tables (CLAUDE.md §72; ADR-0019 T3): every parser, fed truncated, mutated and generated
//! tables, must return, never panic, and hand back only bytes inside the table it was given.
//!
//! Three sources of input, each deterministic so a failure is reproduced by its seed and case:
//! every prefix of every valid sample (its length and checksum repaired, so the parser behind the
//! header check is reached); one to four mutations of a sample, repaired three times in four; and
//! tables generated from nothing but a signature, whose structures have types drawn from those the
//! table defines and lengths, offsets and counts drawn towards the boundaries. The counts are
//! multiplied by `ORIGIN_MUTATION_SCALE` for a long soak.

use crate::census::{self, Class, Seen};
use crate::testing::{Rng, cases, mutate, seal};

/// Every valid sample table of every module.
fn samples() -> Vec<Vec<u8>> {
    [
        crate::madt::tests::samples(),
        crate::fadt::tests::samples(),
        crate::definition::tests::samples(),
        crate::root::tests::samples(),
        crate::srat::tests::samples(),
        crate::platform::tests::samples(),
        crate::ras::tests::samples(),
        crate::memory::tests::samples(),
        crate::boot::tests::samples(),
        crate::nfit::tests::samples(),
        crate::nhlt::tests::samples(),
        crate::sdev::tests::samples(),
        crate::hmat::tests::samples(),
        crate::pptt::tests::samples(),
        crate::viot::tests::samples(),
        crate::records::tests::samples(),
    ]
    .concat()
}

/// Runs the census over `table` and checks what it saw: every structure and run of bytes it
/// handed on lies inside the table, at the offset it gave, and there are no more of them than the
/// table has bytes. Returns the census's result.
fn checked(table: &[u8]) -> Result<census::Census, crate::Error> {
    let range = table.as_ptr_range();
    let mut seen = 0usize;
    let result = census::check_with(table, &mut |s: Seen<'_>| {
        seen += 1;
        let r = s.bytes.as_ptr_range();
        assert!(
            range.start <= r.start && r.end <= range.end,
            "a slice outside the table: {:?} at {:#x}",
            s.kind,
            s.at
        );
        assert!(s.at <= table.len(), "an offset past the table: {:#x}", s.at);
    });
    assert!(
        seen <= table.len().max(1),
        "{seen} structures in {} bytes",
        table.len()
    );
    // A refusal names a place in the table; only a field the table ends before can be past it.
    if let Err(e) = &result {
        match e.kind {
            crate::ErrorKind::Truncated { need } => assert!(e.at < need.max(1), "{e:?}"),
            _ => assert!(e.at <= table.len(), "a refusal past the table: {e:?}"),
        }
    }
    result
}

#[test]
fn every_sample_is_checked_whole() {
    let all = samples();
    assert!(all.len() >= 30, "{} samples", all.len());
    for t in &all {
        let c = checked(t).unwrap_or_else(|e| panic!("{e:?}"));
        assert!(
            matches!(c.class, Class::Checked(_)),
            "{:?}",
            c.header.signature
        );
    }
}

#[test]
fn every_prefix_is_refused_or_read_inside_it() {
    for t in samples() {
        for len in 0..t.len() {
            let mut cut = t[..len].to_vec();
            // Unrepaired: the header's length no longer matches.
            assert!(census::check(&cut).is_err());
            // Repaired, so the table's own parser sees the short body.
            seal(&mut cut);
            let _ = checked(&cut);
        }
    }
}

#[test]
fn mutated_samples_never_escape() {
    let all = samples();
    let mut rng = Rng::new(0x4143_5049_5441_424C);
    for case in 0..cases(40_000) {
        let mut t = all[rng.below(all.len())].clone();
        mutate(&mut t, &mut rng);
        if !rng.chance(4) {
            seal(&mut t);
        }
        let _ = std::panic::catch_unwind(|| checked(&t))
            .unwrap_or_else(|_| panic!("case {case}: {t:02x?}"));
    }
}

/// The signatures whose bodies are runs of typed structures, the fixed bytes before the first,
/// the structure types to draw from and how the type and length are laid out: (type width, length
/// offset, length width).
type Generated = (
    &'static [u8; 4],
    usize,
    &'static [u16],
    (usize, usize, usize),
);

const GENERATED: &[Generated] = &[
    (
        b"APIC",
        8,
        &[0, 1, 2, 3, 4, 5, 9, 10, 16, 11, 0x80],
        (1, 1, 1),
    ),
    (b"SRAT", 12, &[0, 1, 2, 5, 6, 3, 9], (1, 1, 1)),
    (b"CPEP", 8, &[0, 1], (1, 1, 1)),
    (b"NFIT", 4, &[0, 1, 2, 3, 4, 5, 6, 7, 8], (2, 2, 2)),
    (b"HMAT", 4, &[0, 1, 2, 3], (2, 4, 4)),
    (b"SDEV", 0, &[0, 1, 2], (1, 2, 2)),
    (b"PPTT", 0, &[0, 1, 2], (1, 1, 1)),
    (b"FPDT", 0, &[0, 1, 2, 3, 4], (2, 2, 1)),
    (b"PHAT", 0, &[0, 1, 2], (2, 2, 2)),
    (b"VIOT", 12, &[1, 2, 3, 4, 0], (1, 2, 2)),
    (b"PMTT", 4, &[0, 1, 2, 0xFF, 3], (1, 2, 2)),
    (b"MSCT", 20, &[1], (1, 1, 1)),
];

/// A field of `width` bytes holding `value`'s low bytes.
fn put(out: &mut [u8], at: usize, width: usize, value: u64) {
    for (b, v) in out.iter_mut().skip(at).zip(value.to_le_bytes()).take(width) {
        *b = v;
    }
}

/// A table of `signature` with random structures whose sizes, offsets and counts sit near the
/// boundaries.
fn generate(
    rng: &mut Rng,
    signature: &[u8; 4],
    fixed: usize,
    kinds: &[u16],
    shape: (usize, usize, usize),
) -> Vec<u8> {
    let n = rng.below(12);
    generate_n(rng, signature, fixed, kinds, shape, n)
}

/// [`generate`] with `n` structures.
fn generate_n(
    rng: &mut Rng,
    signature: &[u8; 4],
    fixed: usize,
    kinds: &[u16],
    shape: (usize, usize, usize),
    n: usize,
) -> Vec<u8> {
    let (kind_width, len_at, len_width) = shape;
    let mut body: Vec<u8> = (0..fixed)
        .map(|_| if rng.chance(3) { rng.byte() } else { 0 })
        .collect();
    // Offsets and counts in the fixed part: sometimes a boundary value at a random field.
    if fixed >= 4 && rng.chance(2) {
        let at = rng.below(fixed / 4) * 4;
        put(&mut body, at, 4, rng.edge_len(fixed + 36));
    }
    for _ in 0..n {
        let kind = u64::from(kinds[rng.below(kinds.len())]);
        let len = if rng.chance(5) {
            rng.edge_len(64) as usize & 0x3FF
        } else {
            len_at + len_width + rng.below(96)
        };
        let mut s = vec![0u8; len.max(len_at + len_width)];
        for b in s.iter_mut().skip(len_at + len_width) {
            *b = if rng.chance(2) { rng.byte() } else { 0 };
        }
        // Embedded offsets and counts: boundary values at random aligned places.
        for _ in 0..rng.below(4) {
            if s.len() >= 12 {
                let at = 4 + rng.below((s.len() - 8) / 4) * 4;
                put(
                    &mut s,
                    at,
                    [1, 2, 4][rng.below(3)],
                    rng.edge_len(fixed + 36 + len),
                );
            }
        }
        put(&mut s, 0, kind_width, kind);
        put(&mut s, len_at, len_width, len as u64);
        s.truncate(len.max(1));
        body.extend_from_slice(&s);
    }
    let mut t = crate::testing::table(signature, rng.below(4) as u8, &body);
    if signature == b"VIOT" && t.len() >= 40 {
        // The node count and offset, drawn towards the real nodes.
        put(&mut t, 36, 2, rng.below(8) as u64);
        let offset = if rng.chance(2) {
            48
        } else {
            rng.edge_len(t.len())
        };
        put(&mut t, 38, 2, offset);
        seal(&mut t);
    }
    if signature == b"PMTT" && t.len() >= 40 {
        put(&mut t, 36, 4, rng.below(4) as u64);
        seal(&mut t);
    }
    if signature == b"MSCT" && t.len() >= 40 {
        let offset = if rng.chance(2) {
            56
        } else {
            rng.edge_len(t.len())
        };
        put(&mut t, 36, 4, offset);
        seal(&mut t);
    }
    t
}

#[test]
fn generated_tables_never_escape() {
    let mut rng = Rng::new(0x484F_5354_494C_4521);
    let mut accepted = 0usize;
    // Per signature: tables tried and accepted (the review's F13).
    let mut by_signature = vec![(0usize, 0usize); GENERATED.len()];
    for case in 0..cases(40_000) {
        let which = rng.below(GENERATED.len());
        let (sig, fixed, kinds, shape) = GENERATED[which];
        let t = generate(&mut rng, sig, fixed, kinds, shape);
        let result = std::panic::catch_unwind(|| checked(&t))
            .unwrap_or_else(|_| panic!("case {case}: {t:02x?}"));
        by_signature[which].0 += 1;
        if result.is_ok() {
            accepted += 1;
            by_signature[which].1 += 1;
        }
    }
    // The generator must reach the parsers' acceptance paths too, not only their refusals: in all,
    // and for every signature it generates.
    assert!(accepted > cases(40_000) / 50, "{accepted} accepted");
    // The floor is 5 % of each signature's tables, which every one but the MSCT clears (at the
    // fixed seed, 2026-11-14: FPDT 7 %, the lowest, up to APIC 61 %). The MSCT's ordering and
    // coverage rules leave a random one rarely valid (3 of 3,250), so its acceptance paths rest on
    // its deterministic tests, and its floor is one.
    for (i, &(tried, ok)) in by_signature.iter().enumerate() {
        let sig = GENERATED[i].0;
        let floor = if sig == b"MSCT" { 1 } else { tried / 20 };
        assert!(
            ok >= floor,
            "{}: {ok} of {tried} accepted",
            String::from_utf8_lossy(sig)
        );
    }
}

#[test]
fn tables_without_a_header_never_escape() {
    use crate::boot::{PerformanceKind, PerformanceTable};
    use crate::facs::Facs;
    use crate::memory::MpstChannel;
    use crate::ras::{Ras2Channel, RasfChannel};
    let mut fbpt = b"FBPT".to_vec();
    fbpt.extend_from_slice(&56u32.to_le_bytes());
    fbpt.extend_from_slice(&[2, 0, 48, 2]);
    fbpt.resize(56, 7);
    let mut channel = vec![0u8; 96];
    channel[..4].copy_from_slice(&RasfChannel::SIGNATURE.to_le_bytes());
    channel[42] = 1;
    channel[48..54].copy_from_slice(&[0, 0, 0, 0, 48, 0]);
    let seeds = [crate::facs::tests::facs(), fbpt, channel];
    let mut rng = Rng::new(0x4E4F_4845_4144_4552);
    for case in 0..cases(20_000) {
        let mut b = seeds[rng.below(seeds.len())].clone();
        mutate(&mut b, &mut rng);
        if rng.chance(2) && b.len() >= 8 {
            let len = u32::try_from(b.len()).unwrap();
            b[4..8].copy_from_slice(&len.to_le_bytes());
        }
        let r = std::panic::catch_unwind(|| {
            let _ = Facs::parse(&b);
            for kind in [
                PerformanceKind::Fbpt,
                PerformanceKind::S3pt,
                PerformanceKind::Mbpt,
            ] {
                if let Ok(t) = PerformanceTable::parse(&b, kind) {
                    for r in t.records() {
                        let _ = r;
                    }
                }
            }
            if let Ok(c) = RasfChannel::parse(&b) {
                for blk in c.blocks() {
                    let _ = blk;
                }
            }
            let _ = Ras2Channel::parse(&b, 0);
            let _ = MpstChannel::parse(&b, 0);
        });
        assert!(r.is_ok(), "case {case}: {b:02x?}");
    }
}

/// A large table of `sample`'s signature: its fixed part once, then its structures repeated
/// `copies` times, the counts VIOT and PMTT keep set to match. The repeats' references point into
/// the first copy, so the parsers' acceptance paths and lookups run on every structure. Two
/// tables' rules between structures would refuse a plain repeat at its first copy, so the copies
/// are made to keep them: the MSCT's domain ranges are moved up past the first copy's, and the
/// FPDT's later copies take a reserved record type (4), which allows any number.
fn replicate(sample: &[u8], fixed: usize, copies: usize) -> (Vec<u8>, usize) {
    let body = &sample[36..];
    let (head, structures) = body.split_at(fixed);
    let per = match census::check(sample).map(|c| c.class) {
        Ok(Class::Checked(s)) => s.structures() as usize,
        _ => 0,
    };
    let mut t = crate::testing::table(
        &sample[..4].try_into().unwrap(),
        sample[8],
        &[head, &structures.repeat(copies)].concat(),
    );
    let n = per * copies;
    let first = 36 + head.len();
    let width = structures.len();
    for k in 1..copies {
        let at = first + k * width;
        match &sample[..4] {
            // Every copy's ranges above the last one's, so they ascend without a gap.
            b"MSCT" => {
                // Each structure's length is its second byte; its range's low and high at 2 and 6.
                let starts = |base: usize| {
                    let mut r = base;
                    let mut v = Vec::new();
                    while r + 10 <= base + width && t[r + 1] >= 10 {
                        v.push(r);
                        r += usize::from(t[r + 1]);
                    }
                    v
                };
                let u32_at =
                    |t: &[u8], f: usize| u32::from_le_bytes(t[f..f + 4].try_into().unwrap());
                let stride = starts(first)
                    .iter()
                    .map(|&r| u32_at(&t, r + 6))
                    .max()
                    .unwrap_or(0)
                    + 1;
                for r in starts(at) {
                    for f in [r + 2, r + 6] {
                        let v = u32_at(&t, f) + k as u32 * stride;
                        put(&mut t, f, 4, u64::from(v));
                    }
                }
            }
            b"FPDT" => {
                let mut r = at;
                while r + 3 <= at + width {
                    put(&mut t, r, 2, 4);
                    r += usize::from(t[r + 2]).max(1);
                }
            }
            _ => {}
        }
    }
    match &sample[..4] {
        b"VIOT" => put(&mut t, 36, 2, n as u64),
        b"PMTT" => put(
            &mut t,
            36,
            4,
            copies as u64 * u64::from(u32::from_le_bytes(sample[36..40].try_into().unwrap())),
        ),
        _ => {}
    }
    seal(&mut t);
    (t, n)
}

/// The most walk steps a parse and census of a table of `n` structures with signature `sig` may
/// take (TASKS item 71, the review's F13). Most tables are walked a few times whole: at most four
/// steps a structure. The PPTT and the VIOT look structures up through an index, each lookup at
/// most `n.div_ceil(64) + 1` steps: the PPTT [`crate::pptt::MAX_LOOKUPS`] lookups a structure, the
/// VIOT one. The NFIT is bounded whatever its length ([`crate::nfit::MAX_STEPS`]).
fn step_bound(sig: &[u8], n: usize) -> u64 {
    let lookup = n.div_ceil(64) + 1;
    let bound = match sig {
        b"PPTT" => 4 * n + n * crate::pptt::MAX_LOOKUPS * lookup,
        b"VIOT" => 4 * n + n * lookup,
        b"NFIT" => crate::nfit::MAX_STEPS + 4 * n,
        _ => 4 * n,
    };
    bound as u64 + 16
}

/// The review's F13: a large table of every signature the soak generates, built from that table's
/// valid sample so its acceptance paths and lookups run, costs no more walk steps than its bound.
#[test]
fn large_tables_cost_no_more_than_their_bounds() {
    let mut seen = Vec::new();
    for sample in samples() {
        let Some(&(sig, fixed, ..)) = GENERATED.iter().find(|g| g.0[..] == sample[..4]) else {
            continue;
        };
        for copies in [1usize, 400] {
            let (t, n) = replicate(&sample, fixed, copies);
            crate::walk::steps::take();
            let _ = checked(&t);
            let steps = crate::walk::steps::take();
            let bound = step_bound(sig, n);
            assert!(
                steps <= bound,
                "{}: {steps} steps for {n} structures, past {bound}",
                String::from_utf8_lossy(sig)
            );
        }
        seen.push(sig);
    }
    // Every generated signature has a sample, so each was measured large.
    for &(sig, ..) in GENERATED {
        assert!(seen.contains(&sig), "{}", String::from_utf8_lossy(sig));
    }
}

/// The figures behind [`large_tables_cost_no_more_than_their_bounds`] (development aid).
#[test]
#[ignore = "prints figures; the bound test asserts them"]
fn large_tables_measured() {
    for sample in samples() {
        let Some(&(sig, fixed, ..)) = GENERATED.iter().find(|g| g.0[..] == sample[..4]) else {
            continue;
        };
        for copies in [100usize, 400] {
            let (t, n) = replicate(&sample, fixed, copies);
            crate::walk::steps::take();
            let r = checked(&t);
            let steps = crate::walk::steps::take();
            eprintln!(
                "{} copies={copies} structures={n} len={} steps={steps} bound={} refused={:?}",
                String::from_utf8_lossy(sig),
                t.len(),
                step_bound(sig, n),
                r.err().map(|e| e.kind)
            );
        }
    }
}
