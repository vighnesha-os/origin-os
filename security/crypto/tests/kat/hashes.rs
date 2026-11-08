//! NIST ACVP vectors for SHA-256, SHA-512, SHA3-256, SHA3-512, SHAKE128 and SHAKE256, run through
//! the public one-shot and streaming interfaces.
//!
//! Test types (`draft-celi-acvp-sha`, `draft-celi-acvp-sha3`):
//! - AFT: known input, known output.
//! - VOT (SHAKE only): one message length, many output lengths.
//! - MCT: 100 chained checkpoints of 1000 iterations each. SHA-2 uses the "alternate" form, in
//!   which the concatenation of the last three digests is cut or zero-padded to the seed's
//!   original length; SHA-3 hashes the previous digest; SHAKE takes the next output length from
//!   the previous digest.
//! - LDT: a repeated 8-byte pattern of 1 to 8 GiB, fed in pieces so that the test stays within a
//!   modest amount of memory. `ORIGIN_KAT_SCALE` (see the crate documentation) limits it.

use crate::support::{GIB, data_lines, large_data_limit, same, unhex};
use origin_crypto::{Sha3_256, Sha3_512, Sha256, Sha512, Shake128, Shake256, ShakeReader};

// ---------------------------------------------------------------------------------------
// SHA-2 and SHA-3

/// A fixed-output hash under test.
trait FixedOutput: Sized {
    fn one_shot(data: &[u8]) -> Vec<u8>;
    fn start() -> Self;
    fn absorb(&mut self, data: &[u8]);
    fn finish(self) -> Vec<u8>;
}

macro_rules! fixed_output {
    ($($hash:ty),+) => {$(
        impl FixedOutput for $hash {
            fn one_shot(data: &[u8]) -> Vec<u8> {
                <$hash>::digest(data).to_vec()
            }
            fn start() -> Self {
                <$hash>::new()
            }
            fn absorb(&mut self, data: &[u8]) {
                self.update(data);
            }
            fn finish(self) -> Vec<u8> {
                self.finalize().to_vec()
            }
        }
    )+};
}
fixed_output!(Sha256, Sha512, Sha3_256, Sha3_512);

#[derive(Clone)]
struct Aft {
    tc: u32,
    msg: Vec<u8>,
    md: Vec<u8>,
}

#[derive(Clone, Copy)]
enum MctForm {
    Alternate,
    Standard,
}

#[derive(Clone)]
struct Mct {
    tc: u32,
    form: MctForm,
    seed: Vec<u8>,
    checkpoints: Vec<Vec<u8>>,
}

#[derive(Clone)]
struct Ldt {
    tc: u32,
    content: Vec<u8>,
    bytes: u64,
    md: Vec<u8>,
}

struct FixedVectors {
    aft: Vec<Aft>,
    mct: Vec<Mct>,
    ldt: Vec<Ldt>,
}

fn parse_fixed(file: &'static str) -> FixedVectors {
    let mut out = FixedVectors {
        aft: Vec::new(),
        mct: Vec::new(),
        ldt: Vec::new(),
    };
    for line in data_lines(file) {
        let cols: Vec<&str> = line.split(' ').collect();
        match cols.as_slice() {
            ["aft", tc, msg, md] => out.aft.push(Aft {
                tc: tc.parse().unwrap(),
                msg: unhex(msg),
                md: unhex(md),
            }),
            ["mct", tc, form, seed] => out.mct.push(Mct {
                tc: tc.parse().unwrap(),
                form: match *form {
                    "alternate" => MctForm::Alternate,
                    "standard" => MctForm::Standard,
                    other => panic!("unknown MCT form {other}"),
                },
                seed: unhex(seed),
                checkpoints: Vec::new(),
            }),
            ["res", md] => out.mct.last_mut().unwrap().checkpoints.push(unhex(md)),
            ["ldt", tc, content, bytes, md] => out.ldt.push(Ldt {
                tc: tc.parse().unwrap(),
                content: unhex(content),
                bytes: bytes.parse().unwrap(),
                md: unhex(md),
            }),
            _ => panic!("malformed line: {}", &line[..line.len().min(60)]),
        }
    }
    out
}

fn check_aft<H: FixedOutput>(v: &Aft) -> Result<(), String> {
    same(&H::one_shot(&v.msg), &v.md).map_err(|e| format!("AFT tcId {}: {e}", v.tc))
}

/// The ACVP alternate Monte Carlo test for SHA-2.
fn sha2_alternate_checkpoints<H: FixedOutput>(initial_seed: &[u8]) -> Vec<Vec<u8>> {
    let mut seed = initial_seed.to_vec();
    (0..100)
        .map(|_| {
            let (mut a, mut b, mut c) = (seed.clone(), seed.clone(), seed.clone());
            let mut md = Vec::new();
            for _ in 0..1000 {
                let mut msg = [a.as_slice(), b.as_slice(), c.as_slice()].concat();
                // The leftmost bits of the seed's original length, or zero bits appended.
                msg.resize(initial_seed.len(), 0);
                md = H::one_shot(&msg);
                a = b;
                b = c;
                c = md.clone();
            }
            seed.clone_from(&md);
            md
        })
        .collect()
}

/// The ACVP standard Monte Carlo test for SHA-3.
fn sha3_standard_checkpoints<H: FixedOutput>(initial_seed: &[u8]) -> Vec<Vec<u8>> {
    let mut md = initial_seed.to_vec();
    (0..100)
        .map(|_| {
            for _ in 0..1000 {
                md = H::one_shot(&md);
            }
            md.clone()
        })
        .collect()
}

fn check_mct<H: FixedOutput>(v: &Mct) -> Result<(), String> {
    let got = match v.form {
        MctForm::Alternate => sha2_alternate_checkpoints::<H>(&v.seed),
        MctForm::Standard => sha3_standard_checkpoints::<H>(&v.seed),
    };
    if got.len() != v.checkpoints.len() {
        return Err(format!("MCT tcId {}: wrong number of checkpoints", v.tc));
    }
    for (i, (got, want)) in got.iter().zip(&v.checkpoints).enumerate() {
        same(got, want).map_err(|e| format!("MCT tcId {}, checkpoint {i}: {e}", v.tc))?;
    }
    Ok(())
}

/// Feeds `v.bytes` of the repeated `v.content` to the hash, one megabyte-sized piece at a time.
/// The piece is a whole number of repetitions, so every piece starts at a pattern boundary.
fn check_ldt<H: FixedOutput>(v: &Ldt) -> Result<(), String> {
    const TARGET_PIECE: usize = 1 << 20;
    let piece = v.content.repeat(TARGET_PIECE.div_ceil(v.content.len()));
    let mut remaining = v.bytes;
    let mut hash = H::start();
    while remaining > 0 {
        let take = remaining.min(piece.len() as u64) as usize;
        hash.absorb(&piece[..take]);
        remaining -= take as u64;
    }
    same(&hash.finish(), &v.md).map_err(|e| format!("LDT tcId {}: {e}", v.tc))
}

fn fixed_output_controls<H: FixedOutput>(file: &FixedVectors) {
    // A vector that passes, and fails once its answer or its message is corrupted.
    let good = file.aft.iter().find(|v| !v.msg.is_empty()).unwrap();
    check_aft::<H>(good).unwrap();
    let mut bad = good.clone();
    bad.md[0] ^= 0x01;
    assert!(check_aft::<H>(&bad).is_err(), "corrupted AFT digest");
    let mut bad = good.clone();
    bad.msg[0] ^= 0x01;
    assert!(check_aft::<H>(&bad).is_err(), "corrupted AFT message");

    // The Monte Carlo test, with a corrupted seed and a corrupted last checkpoint.
    let mct = &file.mct[0];
    check_mct::<H>(mct).unwrap();
    let mut bad = mct.clone();
    bad.seed[0] ^= 0x01;
    assert!(check_mct::<H>(&bad).is_err(), "corrupted MCT seed");
    let mut bad = mct.clone();
    bad.checkpoints.last_mut().unwrap()[0] ^= 0x01;
    assert!(check_mct::<H>(&bad).is_err(), "corrupted MCT checkpoint");

    // The piecewise feeding, on a small message known to be equal to its one-shot digest:
    // two whole pieces and five bytes.
    let content = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0];
    let bytes = (2 << 20) + 5;
    let expanded: Vec<u8> = content.iter().cycle().take(bytes).copied().collect();
    let good = Ldt {
        tc: 0,
        content: content.to_vec(),
        bytes: bytes as u64,
        md: H::one_shot(&expanded),
    };
    check_ldt::<H>(&good).unwrap();
    let mut bad = good.clone();
    bad.md[0] ^= 0x01;
    assert!(check_ldt::<H>(&bad).is_err(), "corrupted LDT digest");
    let mut bad = good;
    bad.bytes -= 1;
    assert!(check_ldt::<H>(&bad).is_err(), "LDT message one byte short");
}

macro_rules! fixed_output_kats {
    ($module:ident, $hash:ty, $file:literal, aft: $aft:expr) => {
        mod $module {
            use super::*;

            fn vectors() -> FixedVectors {
                let file = parse_fixed(include_str!(concat!("../vectors/acvp/", $file)));
                assert_eq!(file.aft.len(), $aft, "AFT vectors");
                assert_eq!(file.mct.len(), 1, "MCT vectors");
                let mut sizes: Vec<u64> = file.ldt.iter().map(|v| v.bytes).collect();
                sizes.sort_unstable();
                assert_eq!(sizes, [GIB, 2 * GIB, 4 * GIB, 8 * GIB], "LDT vectors");
                file
            }

            #[test]
            fn aft() {
                for v in &vectors().aft {
                    check_aft::<$hash>(v).unwrap();
                }
            }

            #[test]
            fn monte_carlo() {
                check_mct::<$hash>(&vectors().mct[0]).unwrap();
            }

            #[test]
            fn large_data() {
                let limit = large_data_limit();
                for v in vectors().ldt.iter().filter(|v| v.bytes <= limit) {
                    check_ldt::<$hash>(v).unwrap();
                }
            }

            #[test]
            fn negative_controls() {
                fixed_output_controls::<$hash>(&vectors());
            }
        }
    };
}

fixed_output_kats!(sha2_256, Sha256, "sha2-256.txt", aft: 512);
fixed_output_kats!(sha2_512, Sha512, "sha2-512.txt", aft: 1024);
fixed_output_kats!(sha3_256, Sha3_256, "sha3-256.txt", aft: 151);
fixed_output_kats!(sha3_512, Sha3_512, "sha3-512.txt", aft: 86);

// ---------------------------------------------------------------------------------------
// SHAKE

/// A SHAKE function under test.
trait Xof {
    fn read(data: &[u8]) -> ShakeReader;
}

macro_rules! xof {
    ($($xof:ty),+) => {$(
        impl Xof for $xof {
            fn read(data: &[u8]) -> ShakeReader {
                let mut hash = <$xof>::new();
                hash.update(data);
                hash.finalize_xof()
            }
        }
    )+};
}
xof!(Shake128, Shake256);

/// How a vector file writes the last byte of a digest that is not a whole number of bytes.
/// FIPS 202 output bits fill each byte from its low-order end.
#[derive(Clone, Copy)]
enum PartialByte {
    /// Revision 1.0: the bits stay where FIPS 202 puts them, in the low-order positions.
    LowBits,
    /// Revision FIPS202: the same bits, moved to the high-order end of the byte.
    LeftAligned,
}

#[derive(Clone)]
struct XofCase {
    tc: u32,
    msg: Vec<u8>,
    bits: usize,
    md: Vec<u8>,
}

#[derive(Clone)]
struct XofMct {
    tc: u32,
    seed: Vec<u8>,
    min_bits: usize,
    max_bits: usize,
    checkpoints: Vec<(usize, Vec<u8>)>,
}

struct XofVectors {
    aft: Vec<XofCase>,
    vot: Vec<XofCase>,
    mct: Vec<XofMct>,
}

fn parse_xof(file: &'static str) -> XofVectors {
    let mut out = XofVectors {
        aft: Vec::new(),
        vot: Vec::new(),
        mct: Vec::new(),
    };
    let case = |tc: &str, msg: &str, bits: &str, md: &str| XofCase {
        tc: tc.parse().unwrap(),
        msg: unhex(msg),
        bits: bits.parse().unwrap(),
        md: unhex(md),
    };
    for line in data_lines(file) {
        let cols: Vec<&str> = line.split(' ').collect();
        match cols.as_slice() {
            ["aft", tc, msg, bits, md] => out.aft.push(case(tc, msg, bits, md)),
            ["vot", tc, msg, bits, md] => out.vot.push(case(tc, msg, bits, md)),
            ["mct", tc, seed, min_bits, max_bits] => out.mct.push(XofMct {
                tc: tc.parse().unwrap(),
                seed: unhex(seed),
                min_bits: min_bits.parse().unwrap(),
                max_bits: max_bits.parse().unwrap(),
                checkpoints: Vec::new(),
            }),
            ["res", bits, md] => out
                .mct
                .last_mut()
                .unwrap()
                .checkpoints
                .push((bits.parse().unwrap(), unhex(md))),
            _ => panic!("malformed line: {}", &line[..line.len().min(60)]),
        }
    }
    out
}

/// The first `bits` bits of the XOF's output, in the vector file's representation.
fn xof_output<X: Xof>(msg: &[u8], bits: usize, partial: PartialByte) -> Vec<u8> {
    let mut out = vec![0u8; bits.div_ceil(8)];
    X::read(msg).squeeze(&mut out);
    let kept = bits % 8;
    if kept != 0 {
        let last = out.last_mut().unwrap();
        *last = match partial {
            PartialByte::LowBits => *last & ((1 << kept) - 1),
            PartialByte::LeftAligned => *last << (8 - kept),
        };
    }
    out
}

fn check_xof_case<X: Xof>(v: &XofCase, partial: PartialByte) -> Result<(), String> {
    same(&xof_output::<X>(&v.msg, v.bits, partial), &v.md)
        .map_err(|e| format!("tcId {} ({} bits): {e}", v.tc, v.bits))
}

/// The ACVP Monte Carlo test for SHAKE (revision 1.0).
fn xof_checkpoints<X: Xof>(v: &XofMct) -> Vec<(usize, Vec<u8>)> {
    let (min_bytes, max_bytes) = (v.min_bits / 8, v.max_bits / 8);
    let range = max_bytes - min_bytes + 1;
    let mut out_len = max_bytes;
    let mut md = v.seed.clone();
    (0..100)
        .map(|_| {
            for _ in 0..1000 {
                // The leftmost 128 bits of the previous digest, padded with zeros if shorter.
                let mut msg = md.clone();
                msg.resize(16, 0);
                md = vec![0u8; out_len];
                X::read(&msg).squeeze(&mut md);
                let [.., high, low] = md.as_slice() else {
                    panic!("output of {out_len} bytes");
                };
                out_len = min_bytes + usize::from(u16::from_be_bytes([*high, *low])) % range;
            }
            (md.len() * 8, md.clone())
        })
        .collect()
}

fn check_xof_mct<X: Xof>(v: &XofMct) -> Result<(), String> {
    let got = xof_checkpoints::<X>(v);
    if got.len() != v.checkpoints.len() {
        return Err(format!("MCT tcId {}: wrong number of checkpoints", v.tc));
    }
    for (i, ((got_bits, got), (want_bits, want))) in got.iter().zip(&v.checkpoints).enumerate() {
        if got_bits != want_bits {
            return Err(format!(
                "MCT tcId {}, checkpoint {i}: {got_bits} output bits, want {want_bits}",
                v.tc
            ));
        }
        same(got, want).map_err(|e| format!("MCT tcId {}, checkpoint {i}: {e}", v.tc))?;
    }
    Ok(())
}

fn xof_controls<X: Xof>(file: &XofVectors, partial: PartialByte) {
    let cases = || file.aft.iter().chain(&file.vot);
    // A vector that passes, and fails once its answer or its message is corrupted.
    let good = cases().find(|v| !v.msg.is_empty()).unwrap();
    check_xof_case::<X>(good, partial).unwrap();
    let mut bad = good.clone();
    bad.md[0] ^= 0x01;
    assert!(
        check_xof_case::<X>(&bad, partial).is_err(),
        "corrupted digest"
    );
    let mut bad = good.clone();
    bad.msg[0] ^= 0x01;
    assert!(
        check_xof_case::<X>(&bad, partial).is_err(),
        "corrupted message"
    );

    if let Some(v) = cases().find(|v| v.bits % 8 != 0) {
        // Output of a partial last byte. A kept bit that is wrong fails, and so does a bit
        // outside the kept ones, which the strict comparison catches.
        check_xof_case::<X>(v, partial).unwrap();
        let kept = v.bits % 8;
        let (inside, outside) = match partial {
            PartialByte::LowBits => (1 << (kept - 1), 1 << kept),
            PartialByte::LeftAligned => (1 << (8 - kept), 1 << (7 - kept)),
        };
        let mut bad = v.clone();
        *bad.md.last_mut().unwrap() ^= inside;
        assert!(
            check_xof_case::<X>(&bad, partial).is_err(),
            "kept bit wrong"
        );
        let mut bad = v.clone();
        *bad.md.last_mut().unwrap() ^= outside;
        assert!(
            check_xof_case::<X>(&bad, partial).is_err(),
            "unused bit set"
        );

        // Reading the partial byte the other way fails on at least one vector of the set.
        let other = match partial {
            PartialByte::LowBits => PartialByte::LeftAligned,
            PartialByte::LeftAligned => PartialByte::LowBits,
        };
        assert!(
            cases()
                .filter(|v| v.bits % 8 != 0)
                .any(|v| check_xof_case::<X>(v, other).is_err()),
            "the partial-byte representations are told apart"
        );
    }

    if let Some(mct) = file.mct.first() {
        check_xof_mct::<X>(mct).unwrap();
        let mut bad = mct.clone();
        bad.seed[0] ^= 0x01;
        assert!(check_xof_mct::<X>(&bad).is_err(), "corrupted MCT seed");
        let mut bad = mct.clone();
        bad.checkpoints.last_mut().unwrap().1[0] ^= 0x01;
        assert!(check_xof_mct::<X>(&bad).is_err(), "corrupted checkpoint");
        let mut bad = mct.clone();
        bad.checkpoints.last_mut().unwrap().0 += 8;
        assert!(check_xof_mct::<X>(&bad).is_err(), "wrong checkpoint length");
    }
}

macro_rules! xof_kats {
    ($module:ident, $xof:ty, $file:literal, $partial:expr, aft: $aft:expr, vot: $vot:expr, mct: $mct:expr) => {
        mod $module {
            use super::*;

            fn vectors() -> XofVectors {
                let file = parse_xof(include_str!(concat!("../vectors/acvp/", $file)));
                assert_eq!(file.aft.len(), $aft, "AFT vectors");
                assert_eq!(file.vot.len(), $vot, "VOT vectors");
                assert_eq!(file.mct.len(), $mct, "MCT vectors");
                file
            }

            #[test]
            fn aft_and_vot() {
                let file = vectors();
                for v in file.aft.iter().chain(&file.vot) {
                    check_xof_case::<$xof>(v, $partial).unwrap();
                }
            }

            #[test]
            fn monte_carlo() {
                for v in &vectors().mct {
                    check_xof_mct::<$xof>(v).unwrap();
                }
            }

            #[test]
            fn negative_controls() {
                xof_controls::<$xof>(&vectors(), $partial);
            }
        }
    };
}

xof_kats!(shake128_v1, Shake128, "shake128-1.0.txt", PartialByte::LowBits, aft: 174, vot: 512, mct: 1);
xof_kats!(shake256_v1, Shake256, "shake256-1.0.txt", PartialByte::LowBits, aft: 143, vot: 512, mct: 1);
xof_kats!(shake128_fips202, Shake128, "shake128-fips202.txt", PartialByte::LeftAligned, aft: 269, vot: 0, mct: 0);
xof_kats!(shake256_fips202, Shake256, "shake256-fips202.txt", PartialByte::LeftAligned, aft: 237, vot: 0, mct: 0);
