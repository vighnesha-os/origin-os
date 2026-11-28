//! The fuzz harnesses' library (TASKS item 144; owner item 359; CLAUDE.md §72).
//!
//! Each fuzz target in `tests/fuzz/targets` is a few lines over this crate:
//!
//! - **Writers** turn the fuzzer's bytes into well-formed or nearly well-formed inputs, through
//!   an [`arbitrary::Unstructured`]: an ONBF image with its layout and digests sealed
//!   ([`onbf::image`]), an OCBOR-1 document ([`ocbor::item`], [`ocbor::krn_manifest`]), a
//!   signature block ([`signature::block`]) and an AML term list ([`aml::definition_block`]).
//!   A fuzzer that mutates bytes rarely gets past a digest or a length field; these let it
//!   reach the code behind them. Each writer can also break what it writes, so the refusals
//!   are reached too.
//! - **Exercisers** run every reader of untrusted bytes Origin has for one input
//!   ([`onbf::exercise`], [`onbf::exercise_loader`], [`ocbor::exercise`],
//!   [`signature::exercise`], [`aml::exercise`]), and judge it: no panic, and, for what a
//!   parser accepts, the rules of the specification that its public interface shows.
//!
//! Written for Origin from `docs/spec/ONBF.md`, ACPI 6.6 chapter 20 and the crates' public
//! interfaces. No other project's harnesses or grammars were read (CLAUDE.md §3).
//!
//! Fuzzing finds bugs and proves nothing: a run that finds nothing says only that the inputs
//! it reached did not fail (requirements matrix row S73-12).
#![forbid(unsafe_code)]

pub mod aml;
pub mod ocbor;
pub mod onbf;
pub mod signature;

/// Panics unless integer overflow panics in this build.
///
/// `cargo fuzz` builds in release mode with `-Cdebug-assertions`, and rustc ties overflow
/// checks to debug assertions unless told otherwise; Cargo is not told otherwise. Each target
/// calls this once at start-up, so a build that loses the checks stops instead of fuzzing
/// without them. libFuzzer's panic hook aborts the process, so it is set aside while the probe
/// overflows on purpose.
pub fn require_overflow_checks() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let overflowed = std::panic::catch_unwind(|| {
        let x: u8 = std::hint::black_box(u8::MAX);
        std::hint::black_box(x + std::hint::black_box(1u8))
    });
    std::panic::set_hook(hook);
    assert!(
        overflowed.is_err(),
        "overflow checks are off in this fuzz build; the parsers' arithmetic would go unchecked"
    );
}

/// `n` bytes from a fixed generator (xorshift64*) started at `seed`: the same on every run, for
/// the tests and the seed writer (`examples/seeds.rs`).
pub fn fixed_bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        out.extend_from_slice(&s.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes());
    }
    out.truncate(n);
    out
}

#[cfg(test)]
mod tests;
